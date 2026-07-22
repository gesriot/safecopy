package com.safecopy.android

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.net.Uri
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import java.util.concurrent.CopyOnWriteArraySet
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean

class CopyService : Service() {
    private val executor = Executors.newSingleThreadExecutor()
    private val cancelled = AtomicBoolean(false)
    private val running = AtomicBoolean(false)
    private lateinit var notificationManager: NotificationManager
    private var lastNotificationAt = 0L

    override fun onCreate() {
        super.onCreate()
        notificationManager = getSystemService(NotificationManager::class.java)
        notificationManager.createNotificationChannel(
            NotificationChannel(
                CHANNEL_ID,
                "Надёжное копирование",
                NotificationManager.IMPORTANCE_LOW,
            ),
        )
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_CANCEL) {
            if (running.get()) {
                cancelled.set(true)
                updateState { it.copy(status = "Отмена операции…") }
            } else {
                stopSelf(startId)
            }
            return START_NOT_STICKY
        }

        val command = intent ?: run {
            stopSelf(startId)
            return START_NOT_STICKY
        }
        if (!running.compareAndSet(false, true)) {
            updateState { state ->
                state.copy(logs = (state.logs + "[INFO] Повторная команда проигнорирована").takeLast(MAX_LOG_LINES))
            }
            return START_NOT_STICKY
        }
        startForeground(NOTIFICATION_ID, notification("Подготовка", ongoing = true))
        cancelled.set(false)
        executor.execute { execute(command, startId) }
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        if (running.get()) cancelled.set(true)
        executor.shutdownNow()
        super.onDestroy()
    }

    private fun execute(intent: Intent, startId: Int) {
        resetState()
        try {
            val volumeKey = intent.requiredString(EXTRA_VOLUME_KEY)
            val manifestKey = intent.requiredString(EXTRA_MANIFEST_KEY)
            val treeUri = Uri.parse(intent.requiredString(EXTRA_TREE_URI))
            val settings = CopySettings(
                cooldownSeconds = intent.getIntExtra(EXTRA_COOLDOWN, 45),
                maxRetries = intent.getIntExtra(EXTRA_MAX_RETRIES, 3),
                unlimitedRetries = intent.getBooleanExtra(EXTRA_UNLIMITED, true),
                noManifestOnDrive = intent.getBooleanExtra(EXTRA_NO_MANIFEST, true),
                respectGitignore = intent.getBooleanExtra(EXTRA_RESPECT_GITIGNORE, false),
                skipJunk = intent.getBooleanExtra(EXTRA_SKIP_JUNK, false),
            )
            val engine = SafeCopyEngine(
                context = this,
                volumeKey = volumeKey,
                manifestKey = manifestKey,
                treeUri = treeUri,
                settings = settings,
                usbRequiredForVerification = intent.getBooleanExtra(EXTRA_DESTINATION_IS_USB, true),
                cancelled = cancelled,
                emit = ::handleEngineEvent,
            )

            when (intent.action) {
                ACTION_COPY -> {
                    val selection = SourceSelection(
                        uri = Uri.parse(intent.requiredString(EXTRA_SOURCE_URI)),
                        isTree = intent.getBooleanExtra(EXTRA_SOURCE_IS_TREE, false),
                        displayName = intent.requiredString(EXTRA_SOURCE_NAME),
                        includeRoot = intent.getBooleanExtra(EXTRA_SOURCE_INCLUDE_ROOT, false),
                    )
                    val outcome = engine.copy(selection)
                    val message = if (outcome.failedFiles.isEmpty()) {
                        "Скопировано и проверено файлов: ${outcome.records.size}"
                    } else {
                        "Проверено: ${outcome.records.size}; в карантине: ${outcome.failedFiles.size}"
                    }
                    finishSuccessfully(message)
                }
                ACTION_VERIFY -> {
                    val snapshot = ManifestStore(this).load(manifestKey)
                        ?: error("Для этого накопителя ещё нет успешной копии")
                    engine.verify(snapshot)
                    finishSuccessfully("Последняя копия полностью исправна")
                }
                else -> error("Неизвестная команда")
            }
        } catch (_: JobCancelledException) {
            finishCancelled()
        } catch (error: Exception) {
            finishWithError(rootMessage(error))
        } finally {
            stopForeground(STOP_FOREGROUND_DETACH)
            running.set(false)
            stopSelf(startId)
        }
    }

    private fun handleEngineEvent(event: EngineEvent) {
        when (event) {
            is EngineEvent.Phase -> updateState {
                it.copy(phase = event.phase, status = event.status, busy = true)
            }
            is EngineEvent.Progress -> updateState {
                it.copy(
                    completedBytes = event.completedBytes,
                    totalBytes = event.totalBytes,
                    currentFile = event.currentFile,
                    busy = true,
                )
            }
            is EngineEvent.Log -> updateState { state ->
                state.copy(logs = (state.logs + event.message).takeLast(MAX_LOG_LINES))
            }
        }
        val now = System.currentTimeMillis()
        if (event is EngineEvent.Phase || now - lastNotificationAt > 1_000) {
            lastNotificationAt = now
            notificationManager.notify(
                NOTIFICATION_ID,
                notification(currentState.status, currentState.completedBytes, currentState.totalBytes, true),
            )
        }
    }

    private fun resetState() {
        setState(
            JobState(
                phase = JobPhase.PREPARING,
                status = "Подготовка",
                busy = true,
            ),
        )
    }

    private fun finishSuccessfully(message: String) {
        updateState {
            it.copy(
                phase = JobPhase.DONE,
                status = message,
                currentFile = "",
                busy = false,
                logs = (it.logs + "[ГОТОВО] $message").takeLast(MAX_LOG_LINES),
            )
        }
        notificationManager.notify(NOTIFICATION_ID, notification(message, ongoing = false))
    }

    private fun finishCancelled() {
        updateState {
            it.copy(
                phase = JobPhase.CANCELLED,
                status = "Операция отменена",
                currentFile = "",
                busy = false,
                logs = (it.logs + "[ОТМЕНА] Операция остановлена пользователем").takeLast(MAX_LOG_LINES),
            )
        }
        notificationManager.notify(NOTIFICATION_ID, notification("Операция отменена", ongoing = false))
    }

    private fun finishWithError(message: String) {
        updateState {
            it.copy(
                phase = JobPhase.FAILED,
                status = "Ошибка: $message",
                currentFile = "",
                busy = false,
                logs = (it.logs + "[ОШИБКА] $message").takeLast(MAX_LOG_LINES),
            )
        }
        notificationManager.notify(NOTIFICATION_ID, notification("Ошибка копирования", ongoing = false))
    }

    private fun notification(
        text: String,
        completed: Long = 0,
        total: Long = 0,
        ongoing: Boolean,
    ): Notification {
        val openIntent = PendingIntent.getActivity(
            this,
            1,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val cancelIntent = PendingIntent.getService(
            this,
            2,
            Intent(this, CopyService::class.java).setAction(ACTION_CANCEL),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        return Notification.Builder(this, CHANNEL_ID)
            .setSmallIcon(com.safecopy.android.R.drawable.ic_app)
            .setContentTitle("SafeCopy")
            .setContentText(text)
            .setContentIntent(openIntent)
            .setOnlyAlertOnce(true)
            .setOngoing(ongoing)
            .setAutoCancel(!ongoing)
            .apply {
                if (total > 0) {
                    setProgress(1_000, ((completed.coerceIn(0, total) * 1_000) / total).toInt(), false)
                } else if (ongoing) {
                    setProgress(0, 0, true)
                }
                if (ongoing) addAction(Notification.Action.Builder(null, "Остановить", cancelIntent).build())
            }
            .build()
    }

    private fun updateState(transform: (JobState) -> JobState) {
        synchronized(stateLock) {
            currentState = transform(currentState)
        }
        dispatchState()
    }

    private fun setState(state: JobState) {
        synchronized(stateLock) { currentState = state }
        dispatchState()
    }

    private fun dispatchState() {
        val snapshot = currentState
        mainHandler.post { listeners.forEach { listener -> listener(snapshot) } }
    }

    private fun Intent.requiredString(name: String): String =
        getStringExtra(name) ?: error("Нет параметра $name")

    private fun rootMessage(error: Throwable): String {
        var current = error
        while (current.cause != null && current.cause !== current) current = current.cause!!
        return current.message ?: current.javaClass.simpleName
    }

    companion object {
        const val ACTION_COPY = "com.safecopy.android.COPY"
        const val ACTION_VERIFY = "com.safecopy.android.VERIFY"
        const val ACTION_CANCEL = "com.safecopy.android.CANCEL"
        const val EXTRA_VOLUME_KEY = "volume_key"
        const val EXTRA_MANIFEST_KEY = "manifest_key"
        const val EXTRA_TREE_URI = "tree_uri"
        const val EXTRA_SOURCE_URI = "source_uri"
        const val EXTRA_SOURCE_IS_TREE = "source_is_tree"
        const val EXTRA_SOURCE_NAME = "source_name"
        const val EXTRA_SOURCE_INCLUDE_ROOT = "source_include_root"
        const val EXTRA_DESTINATION_IS_USB = "destination_is_usb"
        const val EXTRA_COOLDOWN = "cooldown"
        const val EXTRA_MAX_RETRIES = "max_retries"
        const val EXTRA_UNLIMITED = "unlimited"
        const val EXTRA_NO_MANIFEST = "no_manifest"
        const val EXTRA_RESPECT_GITIGNORE = "respect_gitignore"
        const val EXTRA_SKIP_JUNK = "skip_junk"

        private const val CHANNEL_ID = "safecopy_jobs"
        private const val NOTIFICATION_ID = 7104
        private const val MAX_LOG_LINES = 300
        private val listeners = CopyOnWriteArraySet<(JobState) -> Unit>()
        private val mainHandler = Handler(Looper.getMainLooper())
        private val stateLock = Any()

        @Volatile
        private var currentState = JobState()

        fun observe(listener: (JobState) -> Unit) {
            listeners += listener
            listener(currentState)
        }

        fun removeObserver(listener: (JobState) -> Unit) {
            listeners -= listener
        }

        fun state(): JobState = currentState
    }
}
