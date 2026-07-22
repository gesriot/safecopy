package com.safecopy.android

import android.content.Context
import android.net.Uri
import android.os.ParcelFileDescriptor
import android.system.ErrnoException
import android.system.Os
import android.system.OsConstants
import org.json.JSONObject
import java.io.IOException
import java.security.MessageDigest
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.math.min

class SafeCopyEngine(
    context: Context,
    private val volumeKey: String,
    private val manifestKey: String,
    private val treeUri: Uri,
    private val settings: CopySettings,
    private val usbRequiredForVerification: Boolean,
    private val cancelled: AtomicBoolean,
    private val emit: (EngineEvent) -> Unit,
) {
    private val appContext = context.applicationContext
    private val resolver = appContext.contentResolver
    private val documents = SafDocuments(resolver)
    private val storageLocator = StorageLocator(appContext)
    private val manifestStore = ManifestStore(appContext)
    private val rootUri = documents.rootDocumentUri(treeUri)
    private var quarantineSequence = 0

    fun copy(selection: SourceSelection): CopyOutcome {
        requireColdReadSupport()
        checkReady(requireUsb = true)
        emit(EngineEvent.Phase(JobPhase.PREPARING, "Проверка места назначения"))
        emit(EngineEvent.Log("[INFO] Восстановление backup и очистка незавершённых tmp-файлов"))
        cleanupInterruptedFiles(rootUri)
        runSanityCheck()

        emit(EngineEvent.Phase(JobPhase.PREPARING, "Сканирование источника"))
        if (settings.respectGitignore) {
            emit(EngineEvent.Log("[INFO] Учитываются правила .gitignore внутри источника"))
        }
        if (settings.skipJunk) {
            emit(EngineEvent.Log("[INFO] Кэши и служебные артефакты будут пропущены"))
        }
        val entries = documents.scan(
            selection = selection,
            respectGitignore = settings.respectGitignore,
            skipJunk = settings.skipJunk,
            onWarning = { warning -> emit(EngineEvent.Log("[WARN] $warning")) },
            checkCancelled = ::checkCancelled,
        )
        check(entries.isNotEmpty()) { "В выбранном источнике нет файлов" }
        checkReservedNames(entries)
        val totalBytes = entries.sumOf { it.size }
        val totalWork = doubledWork(totalBytes)
        emit(EngineEvent.Log("[INFO] Найдено файлов: ${entries.size}, ${formatBytes(totalBytes)}"))

        val previous = manifestStore.load(manifestKey)
            ?.takeIf { it.treeUri == treeUri }
            ?.records
            ?.associateBy { recordKey(it.relativeParts) }
            .orEmpty()
        if (previous.isNotEmpty()) {
            emit(EngineEvent.Log("[INFO] Найден checkpoint: ${previous.size} файлов; проверяю resume"))
        }

        val records = mutableListOf<ManifestRecord>()
        val failedFiles = mutableListOf<String>()
        var completedWork = 0L
        var consecutiveFailures = 0

        for (entry in entries) {
            checkReady(requireUsb = true)
            val relative = entry.relativeParts.joinToString("/")
            try {
                val previousRecord = previous[recordKey(entry.relativeParts)]
                val record = if (
                    previousRecord != null &&
                    canResume(entry, previousRecord, completedWork, totalWork, relative)
                ) {
                    emit(EngineEvent.Log("[RESUME OK] $relative уже записан и проверен"))
                    previousRecord
                } else {
                    val result = copyOne(entry, completedWork, totalWork, relative)
                    emit(EngineEvent.Log("[OK] $relative"))
                    ManifestRecord(entry.relativeParts, result.sha256, result.bytes)
                }

                records += record
                completedWork = saturatingAdd(completedWork, doubledWork(entry.size))
                emit(EngineEvent.Progress(completedWork, totalWork, relative))
                try {
                    manifestStore.save(manifestKey, treeUri, records)
                } catch (error: Exception) {
                    throw FatalDestinationException(
                        "Не удалось сохранить checkpoint внутреннего манифеста",
                        error,
                    )
                }
                consecutiveFailures = 0
            } catch (error: JobCancelledException) {
                throw error
            } catch (error: FatalDestinationException) {
                throw error
            } catch (error: CacheDropException) {
                throw FatalDestinationException(error.message ?: "Cold-read недоступен", error)
            } catch (error: Exception) {
                if (isFatalDestination(error)) {
                    throw FatalDestinationException(
                        "Место назначения недоступно: ${rootMessage(error)}",
                        error,
                    )
                }
                consecutiveFailures += 1
                failedFiles += relative
                completedWork = saturatingAdd(completedWork, doubledWork(entry.size))
                emit(EngineEvent.Progress(completedWork, totalWork, relative))
                val attempts = (error as? FileRetriesExhaustedException)?.attempts
                    ?: settings.maxRetries
                writeQuarantine(relative, attempts, rootMessage(error))
                emit(EngineEvent.Log("[QUARANTINE] $relative: ${rootMessage(error)}"))
                if (consecutiveFailures >= CONSECUTIVE_FAILURE_LIMIT) {
                    throw FatalDestinationException(
                        "$consecutiveFailures файлов подряд не скопированы — вероятна проблема устройства",
                        error,
                    )
                }
            }
        }

        check(records.isNotEmpty()) { "Ни один файл не скопирован успешно" }
        runCooldown()
        verifyRecords(records, treeUri, requireUsb = usbRequiredForVerification)

        manifestStore.save(manifestKey, treeUri, records)
        if (!settings.noManifestOnDrive) {
            writeManifestArtifacts(records)
            emit(EngineEvent.Log("[OK] Манифест SHA-256 записан в место назначения"))
        }
        return CopyOutcome(records, failedFiles)
    }

    fun verify(snapshot: ManifestStore.Snapshot) {
        requireColdReadSupport()
        checkReady(requireUsb = usbRequiredForVerification)
        check(snapshot.treeUri == treeUri) {
            "Последняя копия была записана в другую папку"
        }
        check(snapshot.records.isNotEmpty()) { "Внутренний манифест пуст" }
        verifyRecords(snapshot.records, snapshot.treeUri, usbRequiredForVerification)
    }

    private data class CopyResult(val sha256: String, val bytes: Long)

    private fun canResume(
        entry: SourceEntry,
        expected: ManifestRecord,
        completedBeforeFile: Long,
        totalWork: Long,
        displayPath: String,
    ): Boolean {
        return try {
            val destination = documents.resolve(rootUri, entry.relativeParts) ?: return false
            val destinationInfo = documents.info(destination)
            if (destinationInfo.size > 0 && expected.size > 0 && destinationInfo.size != expected.size) {
                return false
            }

            emit(EngineEvent.Phase(JobPhase.COPYING, "Resume: проверка копии $displayPath"))
            val destinationHash = coldHash(destination) { bytes ->
                emit(EngineEvent.Progress(completedBeforeFile + bytes, totalWork, displayPath))
            }
            if (destinationHash != expected.sha256) return false

            emit(EngineEvent.Phase(JobPhase.COPYING, "Resume: проверка источника $displayPath"))
            val sourceHash = hashSource(entry.uri) { bytes ->
                emit(
                    EngineEvent.Progress(
                        saturatingAdd(completedBeforeFile, saturatingAdd(entry.size, bytes)),
                        totalWork,
                        displayPath,
                    ),
                )
            }
            sourceHash == expected.sha256
        } catch (error: JobCancelledException) {
            throw error
        } catch (error: CacheDropException) {
            throw error
        } catch (error: Exception) {
            emit(EngineEvent.Log("[WARN] Resume для $displayPath не применён: ${rootMessage(error)}"))
            false
        }
    }

    private fun copyOne(
        entry: SourceEntry,
        completedBeforeFile: Long,
        totalWork: Long,
        displayPath: String,
    ): CopyResult {
        val parent = documents.ensureDirectory(rootUri, entry.relativeParts.dropLast(1))
        val destinationName = entry.relativeParts.last()
        val retainedTemporaryFiles = mutableListOf<Uri>()
        var attempt = 0

        while (true) {
            checkReady(requireUsb = true)
            attempt += 1
            val tempName = "$destinationName.safecopy.tmp.$attempt"
            documents.findChild(parent, tempName)?.let { documents.delete(it.uri) }
            var tempUri: Uri? = null

            try {
                emit(EngineEvent.Phase(JobPhase.COPYING, "Запись: $displayPath"))
                tempUri = documents.createTemporaryFile(parent, tempName)
                val written = writeSource(entry.uri, tempUri) { bytes ->
                    emit(EngineEvent.Progress(completedBeforeFile + bytes, totalWork, displayPath))
                }
                checkCancelled()

                emit(EngineEvent.Phase(JobPhase.COPYING, "Контрольное чтение: $displayPath"))
                val actual = coldHash(tempUri) { bytes ->
                    emit(
                        EngineEvent.Progress(
                            saturatingAdd(completedBeforeFile, saturatingAdd(entry.size, bytes)),
                            totalWork,
                            displayPath,
                        ),
                    )
                }
                if (actual != written.sha256) {
                    throw HashMismatchException(
                        "SHA-256 не совпал: ожидался ${written.sha256}, получен $actual",
                    )
                }
                promote(parent, tempUri, destinationName)
                retainedTemporaryFiles.forEach(documents::delete)
                return written
            } catch (error: JobCancelledException) {
                tempUri?.let(documents::delete)
                retainedTemporaryFiles.forEach(documents::delete)
                throw error
            } catch (error: CacheDropException) {
                tempUri?.let(documents::delete)
                retainedTemporaryFiles.forEach(documents::delete)
                throw error
            } catch (error: Exception) {
                val sourceFailure = hasCause<SourceReadException>(error)
                if (!sourceFailure && isFatalDestination(error)) {
                    tempUri?.let(documents::delete)
                    retainedTemporaryFiles.forEach(documents::delete)
                    throw FatalDestinationException(
                        "Место назначения недоступно: ${rootMessage(error)}",
                        error,
                    )
                }

                val moreAllowed = if (sourceFailure) {
                    attempt < settings.maxRetries
                } else {
                    settings.unlimitedRetries || attempt < settings.maxRetries
                }
                if (!moreAllowed) {
                    tempUri?.let(documents::delete)
                    retainedTemporaryFiles.forEach(documents::delete)
                    throw FileRetriesExhaustedException(attempt, error)
                }

                if (!sourceFailure && settings.unlimitedRetries && tempUri != null) {
                    retainedTemporaryFiles += tempUri
                } else {
                    tempUri?.let(documents::delete)
                }
                val limit = if (!sourceFailure && settings.unlimitedRetries) {
                    "без лимита"
                } else {
                    settings.maxRetries.toString()
                }
                emit(
                    EngineEvent.Log(
                        "[RETRY] $displayPath — попытка $attempt/$limit: ${rootMessage(error)}",
                    ),
                )
                interruptibleSleep(backoffMillis(attempt))
            }
        }
    }

    private fun writeSource(
        sourceUri: Uri,
        destinationUri: Uri,
        onBytes: (Long) -> Unit,
    ): CopyResult {
        val sourcePfd = try {
            resolver.openFileDescriptor(sourceUri, "r")
                ?: throw IOException("пустой дескриптор источника")
        } catch (error: Exception) {
            throw SourceReadException("Не удалось открыть источник", error)
        }
        val input = ParcelFileDescriptor.AutoCloseInputStream(sourcePfd)
        val destinationPfd = try {
            resolver.openFileDescriptor(destinationUri, "wt")
                ?: throw IOException("пустой дескриптор назначения")
        } catch (error: Exception) {
            runCatching { input.close() }
            throw DestinationException("Не удалось открыть временный файл", error)
        }
        val output = ParcelFileDescriptor.AutoCloseOutputStream(destinationPfd)
        val digest = MessageDigest.getInstance("SHA-256")
        val buffer = ByteArray(BUFFER_SIZE)
        var bytes = 0L

        try {
            while (true) {
                checkCancelled()
                val count = try {
                    input.read(buffer)
                } catch (error: Exception) {
                    throw SourceReadException("Ошибка чтения исходного файла", error)
                }
                if (count < 0) break
                if (count == 0) continue
                try {
                    output.write(buffer, 0, count)
                } catch (error: Exception) {
                    throw DestinationException("Ошибка записи в место назначения", error)
                }
                digest.update(buffer, 0, count)
                bytes += count
                onBytes(bytes)
            }
            try {
                output.flush()
                Os.fsync(destinationPfd.fileDescriptor)
            } catch (error: Exception) {
                throw DestinationException("Не удалось синхронизировать данные", error)
            }
        } finally {
            runCatching { output.close() }
            runCatching { input.close() }
        }
        return CopyResult(digest.digest().toHex(), bytes)
    }

    private fun hashSource(uri: Uri, onBytes: (Long) -> Unit): String {
        val pfd = try {
            resolver.openFileDescriptor(uri, "r")
                ?: throw IOException("пустой дескриптор источника")
        } catch (error: Exception) {
            throw SourceReadException("Не удалось открыть источник", error)
        }
        val input = ParcelFileDescriptor.AutoCloseInputStream(pfd)
        val digest = MessageDigest.getInstance("SHA-256")
        val buffer = ByteArray(BUFFER_SIZE)
        var bytes = 0L
        try {
            while (true) {
                checkCancelled()
                val count = try {
                    input.read(buffer)
                } catch (error: Exception) {
                    throw SourceReadException("Ошибка чтения исходного файла", error)
                }
                if (count < 0) break
                if (count > 0) {
                    digest.update(buffer, 0, count)
                    bytes += count
                    onBytes(bytes)
                }
            }
        } finally {
            runCatching { input.close() }
        }
        return digest.digest().toHex()
    }

    private fun coldHash(uri: Uri, onBytes: (Long) -> Unit = {}): String {
        val pfd = try {
            resolver.openFileDescriptor(uri, "r")
                ?: throw IOException("пустой дескриптор")
        } catch (error: Exception) {
            throw DestinationException("Не удалось открыть файл для проверки", error)
        }
        try {
            NativeIo.dropFileCache(pfd.fd)
        } catch (error: Exception) {
            runCatching { pfd.close() }
            throw error
        }
        val input = ParcelFileDescriptor.AutoCloseInputStream(pfd)
        val digest = MessageDigest.getInstance("SHA-256")
        val buffer = ByteArray(BUFFER_SIZE)
        var bytes = 0L
        try {
            while (true) {
                checkCancelled()
                val count = try {
                    input.read(buffer)
                } catch (error: Exception) {
                    throw DestinationException("Ошибка контрольного чтения", error)
                }
                if (count < 0) break
                if (count > 0) {
                    digest.update(buffer, 0, count)
                    bytes += count
                    onBytes(bytes)
                }
            }
        } finally {
            runCatching { input.close() }
        }
        return digest.digest().toHex()
    }

    private fun promote(parent: Uri, temporary: Uri, finalName: String): Uri {
        val existing = documents.findChild(parent, finalName)
        var backup: Uri? = null
        if (existing != null) {
            val backupName = "$finalName.safecopy.old.${System.currentTimeMillis()}"
            backup = documents.renameExact(existing.uri, backupName)
        }

        return try {
            val finalUri = documents.renameExact(temporary, finalName)
            backup?.let(documents::delete)
            finalUri
        } catch (error: Exception) {
            if (backup != null) runCatching { documents.renameExact(backup, finalName) }
            throw DestinationException("Не удалось безопасно заменить итоговый файл", error)
        }
    }

    private fun runSanityCheck() {
        val name = ".safecopy-sanity.tmp"
        documents.findChild(rootUri, name)?.let { documents.delete(it.uri) }
        val uri = documents.createTemporaryFile(rootUri, name)
        try {
            val expected = writeSanityPattern(uri)
            val actual = coldHash(uri)
            if (expected != actual) {
                throw FatalDestinationException(
                    "Предварительная проверка не пройдена: SHA-256 тестового файла не совпал",
                )
            }
            emit(EngineEvent.Log("[OK] Место назначения прошло тест записи 10 МБ"))
        } finally {
            documents.delete(uri)
        }
    }

    private fun writeSanityPattern(uri: Uri): String {
        val pfd = try {
            resolver.openFileDescriptor(uri, "wt")
                ?: throw IOException("пустой дескриптор тестового файла")
        } catch (error: Exception) {
            throw DestinationException("Не удалось открыть тестовый файл", error)
        }
        val output = ParcelFileDescriptor.AutoCloseOutputStream(pfd)
        val digest = MessageDigest.getInstance("SHA-256")
        val buffer = ByteArray(BUFFER_SIZE)
        var state = 0xDEADBEEFCAFEBABEuL.toLong()
        var written = 0
        try {
            while (written < SANITY_SIZE) {
                checkCancelled()
                state = fillPseudoRandom(buffer, state)
                val count = min(buffer.size, SANITY_SIZE - written)
                output.write(buffer, 0, count)
                digest.update(buffer, 0, count)
                written += count
            }
            output.flush()
            Os.fsync(pfd.fileDescriptor)
        } catch (error: JobCancelledException) {
            throw error
        } catch (error: Exception) {
            throw DestinationException("Тестовая запись не удалась", error)
        } finally {
            runCatching { output.close() }
        }
        return digest.digest().toHex()
    }

    private fun fillPseudoRandom(buffer: ByteArray, initialState: Long): Long {
        var state = initialState
        var offset = 0
        while (offset < buffer.size) {
            state = state xor (state shl 13)
            state = state xor (state ushr 7)
            state = state xor (state shl 17)
            var value = state
            repeat(min(8, buffer.size - offset)) {
                buffer[offset++] = value.toByte()
                value = value ushr 8
            }
        }
        return state
    }

    private fun runCooldown() {
        emit(EngineEvent.Phase(JobPhase.COOLDOWN, "Ожидание перед финальной проверкой"))
        for (remaining in settings.cooldownSeconds downTo 1) {
            checkCancelled()
            emit(
                EngineEvent.Phase(
                    JobPhase.COOLDOWN,
                    "Ожидание перед финальной проверкой: $remaining сек",
                ),
            )
            interruptibleSleep(1_000)
        }
    }

    private fun verifyRecords(
        records: List<ManifestRecord>,
        baseTreeUri: Uri,
        requireUsb: Boolean,
    ) {
        emit(EngineEvent.Phase(JobPhase.VERIFYING, "Финальная проверка данных"))
        val baseRoot = documents.rootDocumentUri(baseTreeUri)
        val total = records.sumOf { it.size }
        var completed = 0L
        for (record in records) {
            checkReady(requireUsb)
            val path = record.relativeParts.joinToString("/")
            val uri = documents.resolve(baseRoot, record.relativeParts)
                ?: throw HashMismatchException("После копирования отсутствует файл: $path")
            val actual = coldHash(uri) { bytes ->
                emit(EngineEvent.Progress(saturatingAdd(completed, bytes), total, path))
            }
            if (actual != record.sha256) {
                throw HashMismatchException("Файл повреждён при финальной проверке: $path")
            }
            completed = saturatingAdd(completed, record.size)
            emit(EngineEvent.Progress(completed, total, path))
            emit(EngineEvent.Log("[VERIFY OK] $path"))
        }
        emit(EngineEvent.Log("[OK] Все ${records.size} файлов прошли финальную проверку"))
    }

    private fun writeManifestArtifacts(records: List<ManifestRecord>) {
        val manifestText = buildString {
            for (record in records) {
                append(record.sha256)
                    .append("  ")
                    .append(record.relativeParts.joinToString("/"))
                    .append('\n')
            }
        }
        writeSmallFile(
            rootUri,
            MANIFEST_NAME,
            manifestText.toByteArray(Charsets.UTF_8),
        )
        val readme = "Этот файл содержит SHA-256 хеши данных, записанных SafeCopy.\n" +
            "Проверка выполняется в приложении кнопкой «Проверить последнюю копию».\n"
        writeSmallFile(rootUri, README_NAME, readme.toByteArray(Charsets.UTF_8))
    }

    private fun writeQuarantine(path: String, attempts: Int, reason: String) {
        val quarantine = documents.createDirectory(rootUri, QUARANTINE_DIR)
        quarantineSequence += 1
        val report = JSONObject()
            .put("source_relative", path)
            .put("reason", reason)
            .put("attempts", attempts)
            .put("timestamp_ms", System.currentTimeMillis())
            .toString(2)
            .plus("\n")
        val name = "quarantine-${System.currentTimeMillis()}-$quarantineSequence.json"
        try {
            writeSmallFile(quarantine, name, report.toByteArray(Charsets.UTF_8))
        } catch (error: Exception) {
            if (isFatalDestination(error)) {
                throw FatalDestinationException("Не удалось записать отчёт карантина", error)
            }
            throw error
        }
    }

    private fun writeSmallFile(parent: Uri, name: String, bytes: ByteArray) {
        val tempName = "$name.safecopy.tmp.1"
        documents.findChild(parent, tempName)?.let { documents.delete(it.uri) }
        val temp = documents.createTemporaryFile(parent, tempName)
        val pfd = try {
            resolver.openFileDescriptor(temp, "wt")
                ?: throw IOException("пустой дескриптор")
        } catch (error: Exception) {
            throw DestinationException("Не удалось записать $name", error)
        }
        val output = ParcelFileDescriptor.AutoCloseOutputStream(pfd)
        try {
            output.write(bytes)
            output.flush()
            Os.fsync(pfd.fileDescriptor)
        } catch (error: Exception) {
            throw DestinationException("Не удалось синхронизировать $name", error)
        } finally {
            runCatching { output.close() }
        }
        promote(parent, temp, name)
    }

    private fun checkReservedNames(entries: List<SourceEntry>) {
        for (entry in entries) {
            check(entry.relativeParts.none { '\n' in it || '\r' in it }) {
                "Имя файла содержит перенос строки"
            }
            if (!settings.noManifestOnDrive) {
                val name = entry.relativeParts.singleOrNull()
                check(name != MANIFEST_NAME && name != README_NAME) {
                    "Источник содержит зарезервированный файл $name"
                }
            }
        }
    }

    private fun cleanupInterruptedFiles(directory: Uri) {
        checkCancelled()
        val children = documents.children(directory)
        for (child in children) {
            checkCancelled()
            if (child.isDirectory) {
                cleanupInterruptedFiles(child.uri)
                continue
            }
            when {
                isTemporaryName(child.name) -> documents.delete(child.uri)
                OLD_PATTERN.containsMatchIn(child.name) -> {
                    val finalName = child.name.substringBefore(".safecopy.old.")
                    if (documents.findChild(directory, finalName) == null) {
                        runCatching { documents.renameExact(child.uri, finalName) }
                    } else {
                        documents.delete(child.uri)
                    }
                }
            }
        }
    }

    private fun isTemporaryName(name: String): Boolean =
        ".safecopy.tmp." in name || ".safecopy.tmp (" in name

    private fun requireColdReadSupport() {
        NativeIo.requireAvailable()
        emit(EngineEvent.Log("[OK] Нативный cache-drop модуль загружен"))
    }

    private fun checkReady(requireUsb: Boolean) {
        checkCancelled()
        if (requireUsb && !storageLocator.isMounted(volumeKey)) {
            throw FatalDestinationException("USB-накопитель отключён")
        }
    }

    private fun checkCancelled() {
        if (cancelled.get() || Thread.currentThread().isInterrupted) {
            throw JobCancelledException()
        }
    }

    private fun interruptibleSleep(milliseconds: Long) {
        var remaining = milliseconds
        while (remaining > 0) {
            checkCancelled()
            val slice = min(remaining, 250)
            try {
                Thread.sleep(slice)
            } catch (_: InterruptedException) {
                Thread.currentThread().interrupt()
                throw JobCancelledException()
            }
            remaining -= slice
        }
    }

    private fun isFatalDestination(error: Throwable): Boolean {
        if (!storageLocator.isMounted(volumeKey)) return true
        var sawErrno = false
        var fatalErrno = false
        var fatalWithoutErrno = false
        var current: Throwable? = error
        while (current != null) {
            when (current) {
                is FatalDestinationException,
                is CacheDropException,
                is SecurityException,
                -> return true
                is SafOperationException -> {
                    fatalWithoutErrno = fatalWithoutErrno || current.fatalWithoutErrno
                }
                is ErrnoException -> {
                    sawErrno = true
                    fatalErrno = fatalErrno || current.errno in FATAL_ERRNOS
                }
            }
            current = current.cause
        }
        return if (sawErrno) fatalErrno else fatalWithoutErrno
    }

    private inline fun <reified T : Throwable> hasCause(error: Throwable): Boolean {
        var current: Throwable? = error
        while (current != null) {
            if (current is T) return true
            current = current.cause
        }
        return false
    }

    private fun rootMessage(error: Throwable): String {
        var current = error
        while (current.cause != null && current.cause !== current) current = current.cause!!
        return current.message ?: current.javaClass.simpleName
    }

    private fun backoffMillis(attempt: Int): Long =
        1_000L shl (attempt - 1).coerceIn(0, 5)

    private fun doubledWork(bytes: Long): Long =
        if (bytes > Long.MAX_VALUE / 2) Long.MAX_VALUE else bytes * 2

    private fun saturatingAdd(left: Long, right: Long): Long =
        if (right > Long.MAX_VALUE - left) Long.MAX_VALUE else left + right

    private fun recordKey(parts: List<String>): String = parts.joinToString("\u0000")

    private fun ByteArray.toHex(): String = joinToString("") {
        (it.toInt() and 0xff).toString(16).padStart(2, '0')
    }

    private fun formatBytes(bytes: Long): String {
        val units = arrayOf("Б", "КБ", "МБ", "ГБ", "ТБ")
        var value = bytes.toDouble()
        var unit = 0
        while (value >= 1024 && unit < units.lastIndex) {
            value /= 1024
            unit += 1
        }
        return if (unit == 0) "$bytes ${units[unit]}" else "%.1f %s".format(value, units[unit])
    }

    companion object {
        private const val BUFFER_SIZE = 1024 * 1024
        private const val SANITY_SIZE = 10 * 1024 * 1024
        private const val CONSECUTIVE_FAILURE_LIMIT = 5
        private const val MANIFEST_NAME = ".safecopy-manifest.sha256"
        private const val README_NAME = ".safecopy-manifest.README.txt"
        private const val QUARANTINE_DIR = ".quarantine"
        private val OLD_PATTERN = Regex("\\.safecopy\\.old\\.\\d+$")
        private val FATAL_ERRNOS = setOf(
            OsConstants.ENOSPC,
            OsConstants.EDQUOT,
            OsConstants.EACCES,
            OsConstants.EPERM,
            OsConstants.EROFS,
            OsConstants.ENODEV,
        )
    }
}
