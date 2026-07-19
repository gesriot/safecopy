package com.safecopy.android

import android.Manifest
import android.annotation.SuppressLint
import android.app.Activity
import android.app.AlertDialog
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.content.res.ColorStateList
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.provider.DocumentsContract
import android.text.method.ScrollingMovementMethod
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.WindowManager
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ProgressBar
import android.widget.ScrollView
import android.widget.SeekBar
import android.widget.Switch
import android.widget.TextView
import java.util.concurrent.Executors

class MainActivity : Activity() {
    private enum class Direction { TO_USB, FROM_USB }

    private lateinit var toUsbButton: Button
    private lateinit var fromUsbButton: Button
    private lateinit var sourceValue: TextView
    private lateinit var sourceAccessButton: Button
    private lateinit var destinationTitle: TextView
    private lateinit var destinationValue: TextView
    private lateinit var accessButton: Button
    private lateinit var fileButton: Button
    private lateinit var folderButton: Button
    private lateinit var startButton: Button
    private lateinit var verifyButton: Button
    private lateinit var cancelButton: Button
    private lateinit var unlimitedSwitch: Switch
    private lateinit var noManifestSwitch: Switch
    private lateinit var cooldownSeek: SeekBar
    private lateinit var cooldownValue: TextView
    private lateinit var retriesSeek: SeekBar
    private lateinit var retriesValue: TextView
    private lateinit var progressBar: ProgressBar
    private lateinit var statusValue: TextView
    private lateinit var currentFileValue: TextView
    private lateinit var logValue: TextView

    private lateinit var storageLocator: StorageLocator
    private val preferences by lazy { getSharedPreferences("ui_settings", MODE_PRIVATE) }
    private val metadataExecutor = Executors.newSingleThreadExecutor()
    private var mountedVolume: StorageLocator.MountedVolume? = null
    private var destinationTreeUri: Uri? = null
    private var phoneSource: SourceSelection? = null
    private var usbSource: SourceSelection? = null
    private var phoneDestinationUri: Uri? = null
    private var phoneDestinationName: String = "Не выбрано"
    private var direction = Direction.TO_USB
    private var pickerDirection = Direction.TO_USB
    private var receiverRegistered = false

    private val stateObserver: (JobState) -> Unit = { state -> renderState(state) }
    private val storageReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context?, intent: Intent?) {
            window.decorView.postDelayed({ refreshStorage() }, 350)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        storageLocator = StorageLocator(this)
        direction = runCatching {
            Direction.valueOf(preferences.getString(KEY_DIRECTION, Direction.TO_USB.name)!!)
        }.getOrDefault(Direction.TO_USB)
        window.statusBarColor = Color.BLACK
        window.navigationBarColor = Color.BLACK
        buildInterface()
        restoreSelections()
        refreshStorage()
        registerStorageReceiver()
        requestNotificationPermissionIfNeeded()
    }

    override fun onStart() {
        super.onStart()
        CopyService.observe(stateObserver)
    }

    override fun onResume() {
        super.onResume()
        refreshStorage()
    }

    override fun onStop() {
        CopyService.removeObserver(stateObserver)
        super.onStop()
    }

    override fun onDestroy() {
        if (receiverRegistered) unregisterReceiver(storageReceiver)
        metadataExecutor.shutdownNow()
        super.onDestroy()
    }

    @Deprecated("Deprecated in Android")
    @SuppressLint("WrongConstant")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (resultCode != RESULT_OK || data?.data == null) return
        val uri = data.data!!
        val flags = data.flags and (
            Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION
        )
        runCatching { contentResolver.takePersistableUriPermission(uri, flags) }

        when (requestCode) {
            REQUEST_FILE -> setSource(uri, isTree = false, pickerDirection)
            REQUEST_FOLDER -> setSource(uri, isTree = true, pickerDirection)
            REQUEST_USB_ACCESS -> acceptUsbAccess(uri)
            REQUEST_PHONE_DESTINATION -> acceptPhoneDestination(uri)
        }
    }

    private fun buildInterface() {
        val scroll = ScrollView(this).apply {
            isFillViewport = true
            setBackgroundColor(COLOR_SURFACE)
        }
        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(20), dp(22), dp(20), dp(32))
        }
        scroll.addView(root, ViewGroup.LayoutParams(MATCH, WRAP))

        root.addView(text("SAFE COPY", 34, COLOR_INK, Typeface.BOLD), margins(bottom = 6))
        root.addView(
            text(
                "Запись с принудительной синхронизацией и двойной проверкой SHA‑256",
                15,
                COLOR_MUTED,
            ),
            margins(bottom = 16),
        )

        val directionRow = horizontalRow()
        toUsbButton = actionButton("На USB", primary = true).apply {
            setOnClickListener { setDirection(Direction.TO_USB) }
        }
        fromUsbButton = actionButton("С USB", primary = false).apply {
            setOnClickListener { setDirection(Direction.FROM_USB) }
        }
        directionRow.addView(toUsbButton, weightedMargins(end = 6))
        directionRow.addView(fromUsbButton, weightedMargins(start = 6))
        root.addView(directionRow, margins(bottom = 14))

        root.addView(card("ИСТОЧНИК") { body ->
            sourceValue = valueText("Не выбрано")
            body.addView(sourceValue, margins(bottom = 12))
            sourceAccessButton = actionButton("Предоставить доступ к USB", primary = false).apply {
                visibility = View.GONE
                setOnClickListener { requestUsbAccess() }
            }
            body.addView(sourceAccessButton, margins(bottom = 12))
            val row = horizontalRow()
            fileButton = actionButton("Файл", primary = false).apply {
                setOnClickListener { chooseFile() }
            }
            folderButton = actionButton("Папка", primary = false).apply {
                setOnClickListener { chooseFolder() }
            }
            row.addView(fileButton, weightedMargins(end = 6))
            row.addView(folderButton, weightedMargins(start = 6))
            body.addView(row)
        }, margins(bottom = 12))

        val destinationCard = card("USB-НАКОПИТЕЛЬ") { body ->
            destinationValue = valueText("Накопитель не подключён")
            body.addView(destinationValue)
            accessButton = actionButton("Предоставить доступ", primary = false).apply {
                visibility = View.GONE
                setOnClickListener { requestUsbAccess() }
            }
            body.addView(accessButton, margins(top = 12))
        }
        destinationTitle = destinationCard.getChildAt(0) as TextView
        root.addView(destinationCard, margins(bottom = 12))

        root.addView(card("НАСТРОЙКИ") { body ->
            unlimitedSwitch = Switch(this).apply {
                text = "Копировать до победного"
                textSize = 15f
                isChecked = preferences.getBoolean(KEY_UNLIMITED, true)
                thumbTintList = ColorStateList.valueOf(COLOR_ACCENT)
                trackTintList = ColorStateList.valueOf(0x66D71920)
                setOnCheckedChangeListener { _, checked ->
                    preferences.edit().putBoolean(KEY_UNLIMITED, checked).apply()
                    retriesSeek.isEnabled = !checked
                    retriesValue.alpha = if (checked) 0.45f else 1f
                }
            }
            body.addView(unlimitedSwitch, ViewGroup.LayoutParams(MATCH, WRAP))

            noManifestSwitch = Switch(this).apply {
                text = "Без манифеста на накопителе"
                textSize = 15f
                isChecked = preferences.getBoolean(KEY_NO_MANIFEST, true)
                thumbTintList = ColorStateList.valueOf(COLOR_ACCENT)
                trackTintList = ColorStateList.valueOf(0x66D71920)
                setOnCheckedChangeListener { _, checked ->
                    preferences.edit().putBoolean(KEY_NO_MANIFEST, checked).apply()
                }
            }
            body.addView(noManifestSwitch, ViewGroup.LayoutParams(MATCH, WRAP))

            cooldownValue = text("Пауза перед проверкой: 45 сек", 14, COLOR_INK)
            body.addView(cooldownValue, margins(top = 12))
            cooldownSeek = SeekBar(this).apply {
                max = 120
                progress = preferences.getInt(KEY_COOLDOWN, 45)
                progressTintList = ColorStateList.valueOf(COLOR_ACCENT)
                thumbTintList = ColorStateList.valueOf(COLOR_ACCENT)
                cooldownValue.text = "Пауза перед проверкой: $progress сек"
                setOnSeekBarChangeListener(simpleSeekListener { value ->
                    cooldownValue.text = "Пауза перед проверкой: $value сек"
                    preferences.edit().putInt(KEY_COOLDOWN, value).apply()
                })
            }
            body.addView(cooldownSeek, ViewGroup.LayoutParams(MATCH, WRAP))

            retriesValue = text("Попыток при ограниченном режиме: 3", 14, COLOR_INK)
            body.addView(retriesValue, margins(top = 8))
            retriesSeek = SeekBar(this).apply {
                max = 9
                progress = preferences.getInt(KEY_MAX_RETRIES, 3) - 1
                progressTintList = ColorStateList.valueOf(COLOR_ACCENT)
                thumbTintList = ColorStateList.valueOf(COLOR_ACCENT)
                isEnabled = !unlimitedSwitch.isChecked
                setOnSeekBarChangeListener(simpleSeekListener { value ->
                    val retries = value + 1
                    retriesValue.text = "Попыток при ограниченном режиме: $retries"
                    preferences.edit().putInt(KEY_MAX_RETRIES, retries).apply()
                })
            }
            retriesValue.alpha = if (unlimitedSwitch.isChecked) 0.45f else 1f
            body.addView(retriesSeek, ViewGroup.LayoutParams(MATCH, WRAP))
        }, margins(bottom = 16))

        startButton = actionButton("НАЧАТЬ КОПИРОВАНИЕ", primary = true).apply {
            minHeight = dp(56)
            setTypeface(typeface, Typeface.BOLD)
            setOnClickListener { startCopy() }
        }
        root.addView(startButton, ViewGroup.LayoutParams(MATCH, dp(56)))

        verifyButton = actionButton("Проверить последнюю копию", primary = false).apply {
            setOnClickListener { startVerification() }
        }
        root.addView(verifyButton, margins(top = 10))

        cancelButton = actionButton("Остановить", primary = false).apply {
            visibility = View.GONE
            setTextColor(COLOR_ERROR)
            setOnClickListener { cancelJob() }
        }
        root.addView(cancelButton, margins(top = 10))

        root.addView(text("СОСТОЯНИЕ", 12, COLOR_MUTED, Typeface.BOLD).apply {
            letterSpacing = 0.12f
        }, margins(top = 24, bottom = 8))
        statusValue = text("Готово к работе", 18, COLOR_INK, Typeface.BOLD)
        root.addView(statusValue)
        currentFileValue = text("", 13, COLOR_MUTED).apply { visibility = View.GONE }
        root.addView(currentFileValue, margins(top = 4))
        progressBar = ProgressBar(this, null, android.R.attr.progressBarStyleHorizontal).apply {
            max = 1_000
            progressTintList = ColorStateList.valueOf(COLOR_ACCENT)
            progressBackgroundTintList = ColorStateList.valueOf(0xFFD8D8D2.toInt())
        }
        root.addView(progressBar, margins(top = 12, bottom = 12, height = 7))
        logValue = text("Журнал появится после запуска", 12, COLOR_MUTED).apply {
            typeface = Typeface.MONOSPACE
            maxLines = 12
            movementMethod = ScrollingMovementMethod()
            setPadding(dp(12), dp(12), dp(12), dp(12))
            background = roundedBackground(0xFFE9E9E4.toInt(), 12f)
        }
        root.addView(logValue, ViewGroup.LayoutParams(MATCH, dp(180)))

        setContentView(scroll)
        renderDirection()
        renderState(CopyService.state())
    }

    private fun chooseFile() {
        pickerDirection = direction
        startActivityForResult(
            Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
                addCategory(Intent.CATEGORY_OPENABLE)
                type = "*/*"
                addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION)
                initialUsbUri()?.let { putExtra(DocumentsContract.EXTRA_INITIAL_URI, it) }
            },
            REQUEST_FILE,
        )
    }

    private fun chooseFolder() {
        pickerDirection = direction
        startActivityForResult(
            Intent(Intent.ACTION_OPEN_DOCUMENT_TREE).apply {
                addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION)
                initialUsbUri()?.let { putExtra(DocumentsContract.EXTRA_INITIAL_URI, it) }
            },
            REQUEST_FOLDER,
        )
    }

    private fun initialUsbUri(): Uri? = if (direction == Direction.FROM_USB) {
        destinationTreeUri?.let { SafDocuments(contentResolver).rootDocumentUri(it) }
    } else {
        null
    }

    private fun setSource(uri: Uri, isTree: Boolean, selectedDirection: Direction) {
        if (selectedDirection == Direction.FROM_USB) {
            val mounted = mountedVolume
            if (mounted == null || !storageLocator.belongsTo(mounted, uri)) {
                showMessage("Выберите файл или папку на обнаруженном USB-накопителе")
                return
            }
        }
        sourceValue.text = "Чтение выбранного источника…"
        metadataExecutor.execute {
            val name = runCatching {
                val docs = SafDocuments(contentResolver)
                if (isTree) docs.info(docs.rootDocumentUri(uri)).name else docs.info(uri).name
            }.getOrDefault(if (isTree) "Выбранная папка" else "Выбранный файл")
            val selection = SourceSelection(uri, isTree, name, includeRoot = isTree)
            runOnUiThread {
                if (isDestroyed) return@runOnUiThread
                if (selectedDirection == Direction.TO_USB) {
                    phoneSource = selection
                    preferences.edit()
                        .putString(KEY_SOURCE_URI, uri.toString())
                        .putBoolean(KEY_SOURCE_TREE, isTree)
                        .putString(KEY_SOURCE_NAME, name)
                        .apply()
                } else {
                    usbSource = selection
                    preferences.edit()
                        .putString(KEY_USB_SOURCE_URI, uri.toString())
                        .putBoolean(KEY_USB_SOURCE_TREE, isTree)
                        .putString(KEY_USB_SOURCE_NAME, name)
                        .putString(KEY_USB_SOURCE_VOLUME, mountedVolume?.key)
                        .apply()
                }
                renderDirection()
            }
        }
    }

    private fun restoreSelections() {
        preferences.getString(KEY_SOURCE_URI, null)?.let(Uri::parse)?.let { uri ->
            phoneSource = SourceSelection(
                uri,
                preferences.getBoolean(KEY_SOURCE_TREE, false),
                preferences.getString(KEY_SOURCE_NAME, null) ?: "Выбранный источник",
                includeRoot = preferences.getBoolean(KEY_SOURCE_TREE, false),
            )
        }
        phoneDestinationUri = preferences.getString(KEY_PHONE_DEST_URI, null)?.let(Uri::parse)
        phoneDestinationName = preferences.getString(KEY_PHONE_DEST_NAME, null) ?: "Не выбрано"
        renderDirection()
    }

    private fun requestUsbAccess() {
        val mounted = mountedVolume ?: return
        val intent = mounted.volume.createOpenDocumentTreeIntent().apply {
            addFlags(
                Intent.FLAG_GRANT_READ_URI_PERMISSION or
                    Intent.FLAG_GRANT_WRITE_URI_PERMISSION or
                    Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION or
                    Intent.FLAG_GRANT_PREFIX_URI_PERMISSION,
            )
        }
        startActivityForResult(intent, REQUEST_USB_ACCESS)
    }

    private fun acceptUsbAccess(uri: Uri) {
        val mounted = mountedVolume ?: return
        if (!storageLocator.belongsTo(mounted, uri)) {
            showMessage("Нужно предоставить доступ к обнаруженному USB-накопителю")
            return
        }
        val flags = Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION
        try {
            contentResolver.takePersistableUriPermission(uri, flags)
            storageLocator.saveAccess(mounted, uri)
        } catch (error: Exception) {
            showMessage("Не удалось сохранить доступ: ${error.message}")
        }
        refreshStorage()
    }

    private fun choosePhoneDestination() {
        startActivityForResult(
            Intent(Intent.ACTION_OPEN_DOCUMENT_TREE).apply {
                addFlags(
                    Intent.FLAG_GRANT_READ_URI_PERMISSION or
                        Intent.FLAG_GRANT_WRITE_URI_PERMISSION or
                        Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION,
                )
            },
            REQUEST_PHONE_DESTINATION,
        )
    }

    private fun acceptPhoneDestination(uri: Uri) {
        val mounted = mountedVolume
        if (mounted != null && storageLocator.belongsTo(mounted, uri)) {
            showMessage("Для обратного копирования выберите папку в памяти смартфона, а не на USB")
            return
        }
        val isPhoneStorage = uri.authority == "com.android.externalstorage.documents" &&
            runCatching {
                DocumentsContract.getTreeDocumentId(uri).substringBefore(':') == "primary"
            }.getOrDefault(false)
        if (!isPhoneStorage) {
            showMessage("Выберите папку во внутренней памяти смартфона")
            return
        }
        val flags = Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION
        try {
            contentResolver.takePersistableUriPermission(uri, flags)
            phoneDestinationUri = uri
            phoneDestinationName = "Чтение папки…"
            renderDirection()
            metadataExecutor.execute {
                val result = runCatching {
                    SafDocuments(contentResolver).info(
                        SafDocuments(contentResolver).rootDocumentUri(uri),
                    ).name
                }
                runOnUiThread {
                    if (isDestroyed) return@runOnUiThread
                    result.onSuccess { name ->
                        phoneDestinationName = name
                        preferences.edit()
                            .putString(KEY_PHONE_DEST_URI, uri.toString())
                            .putString(KEY_PHONE_DEST_NAME, name)
                            .apply()
                    }.onFailure { error ->
                        phoneDestinationUri = null
                        showMessage("Не удалось прочитать папку: ${error.message}")
                    }
                    renderDirection()
                }
            }
        } catch (error: Exception) {
            showMessage("Не удалось сохранить доступ к папке: ${error.message}")
        }
    }

    private fun refreshStorage() {
        mountedVolume = storageLocator.mountedRemovable()
        destinationTreeUri = mountedVolume?.let(storageLocator::accessUri)
        val mounted = mountedVolume
        if (
            mounted != null && usbSource == null &&
            preferences.getString(KEY_USB_SOURCE_VOLUME, null) == mounted.key
        ) {
            preferences.getString(KEY_USB_SOURCE_URI, null)?.let(Uri::parse)?.let { uri ->
                usbSource = SourceSelection(
                    uri,
                    preferences.getBoolean(KEY_USB_SOURCE_TREE, false),
                    preferences.getString(KEY_USB_SOURCE_NAME, null) ?: "Выбранный источник",
                    includeRoot = preferences.getBoolean(KEY_USB_SOURCE_TREE, false),
                )
            }
        }
        renderDirection()
    }

    private fun startCopy() {
        val mounted = mountedVolume ?: return
        val usbTree = destinationTreeUri ?: return
        val source: SourceSelection
        val destination: Uri
        if (direction == Direction.TO_USB) {
            source = phoneSource ?: return
            if (storageLocator.belongsTo(mounted, source.uri)) {
                showMessage("Источник уже находится на USB-накопителе. Выберите данные в памяти смартфона")
                return
            }
            destination = usbTree
        } else {
            source = usbSource ?: SourceSelection(
                usbTree,
                isTree = true,
                displayName = mounted.description,
                includeRoot = false,
            )
            destination = phoneDestinationUri ?: return
            preferences.edit().putString(KEY_LAST_REVERSE_VOLUME, mounted.key).apply()
        }
        val intent = Intent(this, CopyService::class.java).apply {
            action = CopyService.ACTION_COPY
            putExtra(CopyService.EXTRA_VOLUME_KEY, mounted.key)
            putExtra(CopyService.EXTRA_MANIFEST_KEY, manifestKey(mounted.key))
            putExtra(CopyService.EXTRA_TREE_URI, destination.toString())
            putExtra(CopyService.EXTRA_SOURCE_URI, source.uri.toString())
            putExtra(CopyService.EXTRA_SOURCE_IS_TREE, source.isTree)
            putExtra(CopyService.EXTRA_SOURCE_NAME, source.displayName)
            putExtra(CopyService.EXTRA_SOURCE_INCLUDE_ROOT, source.includeRoot)
            putExtra(CopyService.EXTRA_DESTINATION_IS_USB, direction == Direction.TO_USB)
            putExtra(CopyService.EXTRA_COOLDOWN, cooldownSeek.progress)
            putExtra(CopyService.EXTRA_MAX_RETRIES, retriesSeek.progress + 1)
            putExtra(CopyService.EXTRA_UNLIMITED, unlimitedSwitch.isChecked)
            putExtra(CopyService.EXTRA_NO_MANIFEST, noManifestSwitch.isChecked)
        }
        startForegroundService(intent)
    }

    private fun startVerification() {
        val mountedKey = if (direction == Direction.TO_USB) {
            mountedVolume?.key
        } else {
            mountedVolume?.key ?: preferences.getString(KEY_LAST_REVERSE_VOLUME, null)
        } ?: return
        val destination = if (direction == Direction.TO_USB) {
            destinationTreeUri
        } else {
            phoneDestinationUri
        } ?: return
        val intent = Intent(this, CopyService::class.java).apply {
            action = CopyService.ACTION_VERIFY
            putExtra(CopyService.EXTRA_VOLUME_KEY, mountedKey)
            putExtra(CopyService.EXTRA_MANIFEST_KEY, manifestKey(mountedKey))
            putExtra(CopyService.EXTRA_TREE_URI, destination.toString())
            putExtra(CopyService.EXTRA_DESTINATION_IS_USB, direction == Direction.TO_USB)
        }
        startForegroundService(intent)
    }

    private fun cancelJob() {
        startService(Intent(this, CopyService::class.java).setAction(CopyService.ACTION_CANCEL))
    }

    private fun setDirection(newDirection: Direction) {
        if (CopyService.state().busy || direction == newDirection) return
        direction = newDirection
        preferences.edit().putString(KEY_DIRECTION, direction.name).apply()
        renderDirection()
    }

    private fun renderDirection() {
        styleDirectionButton(toUsbButton, direction == Direction.TO_USB)
        styleDirectionButton(fromUsbButton, direction == Direction.FROM_USB)
        val mounted = mountedVolume
        val usbTree = destinationTreeUri
        if (direction == Direction.TO_USB) {
            sourceValue.text = phoneSource?.selectionLabel() ?: "Не выбрано"
            sourceValue.setTextColor(if (phoneSource == null) COLOR_MUTED else COLOR_INK)
            destinationTitle.text = "USB-НАКОПИТЕЛЬ"
            noManifestSwitch.text = "Без манифеста на накопителе"
            when {
                mounted == null -> {
                    destinationValue.text = "USB-накопитель не подключён"
                    destinationValue.setTextColor(COLOR_MUTED)
                }
                usbTree == null -> {
                    destinationValue.text = "${mounted.description} обнаружен\nНужно разрешить доступ один раз"
                    destinationValue.setTextColor(COLOR_WARNING)
                }
                else -> {
                    destinationValue.text = "${mounted.description}\nПодключён и доступен"
                    destinationValue.setTextColor(COLOR_SUCCESS)
                }
            }
        } else {
            destinationTitle.text = "ПАПКА НА СМАРТФОНЕ"
            noManifestSwitch.text = "Без манифеста в папке назначения"
            when {
                mounted == null -> {
                    sourceValue.text = "USB-накопитель не подключён"
                    sourceValue.setTextColor(COLOR_MUTED)
                }
                usbTree == null -> {
                    sourceValue.text = "${mounted.description} обнаружен\nНужно разрешить доступ один раз"
                    sourceValue.setTextColor(COLOR_WARNING)
                }
                usbSource != null -> {
                    sourceValue.text = usbSource!!.selectionLabel()
                    sourceValue.setTextColor(COLOR_INK)
                }
                else -> {
                    sourceValue.text = "Весь накопитель: ${mounted.description}"
                    sourceValue.setTextColor(COLOR_SUCCESS)
                }
            }
            destinationValue.text = if (phoneDestinationUri == null) {
                "Папка не выбрана"
            } else {
                "Память смартфона\n$phoneDestinationName"
            }
            destinationValue.setTextColor(if (phoneDestinationUri == null) COLOR_MUTED else COLOR_SUCCESS)
        }
        updateButtons(CopyService.state())
    }

    private fun SourceSelection.selectionLabel(): String =
        if (isTree) "Папка: $displayName" else "Файл: $displayName"

    private fun styleDirectionButton(button: Button, selected: Boolean) {
        button.setTextColor(if (selected) Color.WHITE else COLOR_INK)
        button.backgroundTintList = ColorStateList.valueOf(
            if (selected) COLOR_INK else 0xFFE5E5E0.toInt(),
        )
    }

    private fun manifestKey(volumeKey: String): String = "${direction.name.lowercase()}_$volumeKey"

    private fun renderState(state: JobState) {
        statusValue.text = state.status
        statusValue.setTextColor(
            when (state.phase) {
                JobPhase.DONE -> COLOR_SUCCESS
                JobPhase.FAILED -> COLOR_ERROR
                JobPhase.CANCELLED -> COLOR_WARNING
                else -> COLOR_INK
            },
        )
        currentFileValue.visibility = if (state.currentFile.isBlank()) View.GONE else View.VISIBLE
        currentFileValue.text = state.currentFile
        progressBar.isIndeterminate = state.busy && state.totalBytes <= 0
        if (state.totalBytes > 0) {
            progressBar.progress = ((state.completedBytes.coerceIn(0, state.totalBytes) * 1_000) / state.totalBytes).toInt()
        } else if (!state.busy) {
            progressBar.progress = if (state.phase == JobPhase.DONE) 1_000 else 0
        }
        logValue.text = if (state.logs.isEmpty()) {
            "Журнал появится после запуска"
        } else {
            state.logs.takeLast(40).joinToString("\n")
        }
        logValue.post { logValue.scrollTo(0, logValue.layout?.height ?: 0) }
        if (state.busy) {
            window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        } else {
            window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        }
        updateButtons(state)
        if (!state.busy && state.phase == JobPhase.DONE) refreshStorage()
    }

    private fun updateButtons(state: JobState) {
        val usbReady = mountedVolume != null && destinationTreeUri != null
        val ready = if (direction == Direction.TO_USB) {
            phoneSource != null && usbReady
        } else {
            usbReady && phoneDestinationUri != null
        }
        startButton.isEnabled = ready && !state.busy
        fileButton.isEnabled = !state.busy && (direction == Direction.TO_USB || usbReady)
        folderButton.isEnabled = !state.busy && (direction == Direction.TO_USB || usbReady)
        toUsbButton.isEnabled = !state.busy
        fromUsbButton.isEnabled = !state.busy
        unlimitedSwitch.isEnabled = !state.busy
        noManifestSwitch.isEnabled = !state.busy
        cooldownSeek.isEnabled = !state.busy
        retriesSeek.isEnabled = !state.busy && !unlimitedSwitch.isChecked
        val verificationVolumeKey = if (direction == Direction.TO_USB) {
            mountedVolume?.key
        } else {
            mountedVolume?.key ?: preferences.getString(KEY_LAST_REVERSE_VOLUME, null)
        }
        val canVerify = verificationVolumeKey?.let {
            ManifestStore(this).hasManifest(manifestKey(it))
        } == true
        val verificationDestination = if (direction == Direction.TO_USB) destinationTreeUri else phoneDestinationUri
        val verificationReady = if (direction == Direction.TO_USB) usbReady else verificationDestination != null
        verifyButton.isEnabled = canVerify && verificationReady && !state.busy
        cancelButton.visibility = if (state.busy) View.VISIBLE else View.GONE
        if (state.busy) {
            accessButton.visibility = View.GONE
            sourceAccessButton.visibility = View.GONE
        } else if (direction == Direction.TO_USB) {
            sourceAccessButton.visibility = View.GONE
            accessButton.text = "Предоставить доступ"
            accessButton.setOnClickListener { requestUsbAccess() }
            accessButton.visibility = if (mountedVolume != null && destinationTreeUri == null) View.VISIBLE else View.GONE
        } else {
            sourceAccessButton.visibility = if (mountedVolume != null && destinationTreeUri == null) View.VISIBLE else View.GONE
            accessButton.text = if (phoneDestinationUri == null) "Выбрать папку" else "Изменить папку"
            accessButton.setOnClickListener { choosePhoneDestination() }
            accessButton.visibility = View.VISIBLE
        }
    }

    private fun registerStorageReceiver() {
        val filter = IntentFilter().apply {
            addAction(Intent.ACTION_MEDIA_MOUNTED)
            addAction(Intent.ACTION_MEDIA_UNMOUNTED)
            addAction(Intent.ACTION_MEDIA_EJECT)
            addAction(Intent.ACTION_MEDIA_REMOVED)
            addAction(Intent.ACTION_MEDIA_BAD_REMOVAL)
            addDataScheme("file")
        }
        if (Build.VERSION.SDK_INT >= 33) {
            registerReceiver(storageReceiver, filter, RECEIVER_NOT_EXPORTED)
        } else {
            @Suppress("DEPRECATION")
            registerReceiver(storageReceiver, filter)
        }
        receiverRegistered = true
    }

    private fun requestNotificationPermissionIfNeeded() {
        if (
            Build.VERSION.SDK_INT >= 33 &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), REQUEST_NOTIFICATIONS)
        }
    }

    private fun showMessage(message: String) {
        AlertDialog.Builder(this)
            .setTitle("SafeCopy")
            .setMessage(message)
            .setPositiveButton("Понятно", null)
            .show()
    }


    private fun card(title: String, build: (LinearLayout) -> Unit): LinearLayout {
        val container = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(18), dp(16), dp(18), dp(18))
            background = roundedBackground(Color.WHITE, 18f, 0xFFD7D7D2.toInt())
        }
        container.addView(text(title, 12, COLOR_MUTED, Typeface.BOLD).apply {
            letterSpacing = 0.12f
        }, margins(bottom = 10))
        build(container)
        return container
    }

    private fun valueText(value: String): TextView = text(value, 17, COLOR_INK, Typeface.BOLD)

    private fun actionButton(label: String, primary: Boolean): Button = Button(this).apply {
        text = label
        textSize = 14f
        isAllCaps = false
        gravity = Gravity.CENTER
        minHeight = dp(48)
        stateListAnimator = null
        setTextColor(if (primary) Color.WHITE else COLOR_INK)
        backgroundTintList = ColorStateList.valueOf(if (primary) COLOR_INK else 0xFFE5E5E0.toInt())
    }

    private fun text(
        value: String,
        sizeSp: Int,
        color: Int,
        style: Int = Typeface.NORMAL,
    ): TextView = TextView(this).apply {
        text = value
        textSize = sizeSp.toFloat()
        setTextColor(color)
        setTypeface(typeface, style)
    }

    private fun horizontalRow() = LinearLayout(this).apply {
        orientation = LinearLayout.HORIZONTAL
        gravity = Gravity.CENTER_VERTICAL
    }

    private fun roundedBackground(color: Int, radiusDp: Float, strokeColor: Int? = null) =
        GradientDrawable().apply {
            setColor(color)
            cornerRadius = dp(radiusDp.toInt()).toFloat()
            if (strokeColor != null) setStroke(dp(1), strokeColor)
        }

    private fun simpleSeekListener(onChange: (Int) -> Unit) = object : SeekBar.OnSeekBarChangeListener {
        override fun onProgressChanged(seekBar: SeekBar?, progress: Int, fromUser: Boolean) {
            if (fromUser) onChange(progress)
        }
        override fun onStartTrackingTouch(seekBar: SeekBar?) = Unit
        override fun onStopTrackingTouch(seekBar: SeekBar?) = Unit
    }

    private fun margins(
        start: Int = 0,
        top: Int = 0,
        end: Int = 0,
        bottom: Int = 0,
        height: Int = WRAP,
    ) = LinearLayout.LayoutParams(MATCH, if (height == WRAP) WRAP else dp(height)).apply {
        setMargins(dp(start), dp(top), dp(end), dp(bottom))
    }

    private fun weightedMargins(start: Int = 0, end: Int = 0) =
        LinearLayout.LayoutParams(0, dp(48), 1f).apply {
            setMargins(dp(start), 0, dp(end), 0)
        }

    private fun dp(value: Int): Int = (value * resources.displayMetrics.density).toInt()

    companion object {
        private const val REQUEST_FILE = 100
        private const val REQUEST_FOLDER = 101
        private const val REQUEST_USB_ACCESS = 102
        private const val REQUEST_NOTIFICATIONS = 103
        private const val REQUEST_PHONE_DESTINATION = 104
        private const val KEY_SOURCE_URI = "source_uri"
        private const val KEY_SOURCE_TREE = "source_tree"
        private const val KEY_SOURCE_NAME = "source_name"
        private const val KEY_USB_SOURCE_URI = "usb_source_uri"
        private const val KEY_USB_SOURCE_TREE = "usb_source_tree"
        private const val KEY_USB_SOURCE_NAME = "usb_source_name"
        private const val KEY_USB_SOURCE_VOLUME = "usb_source_volume"
        private const val KEY_PHONE_DEST_URI = "phone_destination_uri"
        private const val KEY_PHONE_DEST_NAME = "phone_destination_name"
        private const val KEY_DIRECTION = "copy_direction"
        private const val KEY_LAST_REVERSE_VOLUME = "last_reverse_volume"
        private const val KEY_UNLIMITED = "unlimited_retries"
        private const val KEY_NO_MANIFEST = "no_manifest"
        private const val KEY_COOLDOWN = "cooldown"
        private const val KEY_MAX_RETRIES = "max_retries"
        private const val MATCH = ViewGroup.LayoutParams.MATCH_PARENT
        private const val WRAP = ViewGroup.LayoutParams.WRAP_CONTENT
        private const val COLOR_SURFACE = 0xFFF4F4F0.toInt()
        private const val COLOR_INK = 0xFF111111.toInt()
        private const val COLOR_ACCENT = 0xFFD71920.toInt()
        private const val COLOR_MUTED = 0xFF666666.toInt()
        private const val COLOR_SUCCESS = 0xFF187B3A.toInt()
        private const val COLOR_WARNING = 0xFF9A6300.toInt()
        private const val COLOR_ERROR = 0xFFB42318.toInt()
    }
}
