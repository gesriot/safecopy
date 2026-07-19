package com.safecopy.android

import android.net.Uri

data class SourceSelection(
    val uri: Uri,
    val isTree: Boolean,
    val displayName: String,
    val includeRoot: Boolean = isTree,
)

data class SourceEntry(
    val uri: Uri,
    val relativeParts: List<String>,
    val displayName: String,
    val mimeType: String,
    val size: Long,
)

data class CopySettings(
    val cooldownSeconds: Int = 45,
    val maxRetries: Int = 3,
    val unlimitedRetries: Boolean = true,
    val noManifestOnDrive: Boolean = true,
)

data class ManifestRecord(
    val relativeParts: List<String>,
    val sha256: String,
    val size: Long,
)

data class CopyOutcome(
    val records: List<ManifestRecord>,
    val failedFiles: List<String>,
)

enum class JobPhase {
    IDLE,
    PREPARING,
    COPYING,
    COOLDOWN,
    VERIFYING,
    DONE,
    FAILED,
    CANCELLED,
}

data class JobState(
    val phase: JobPhase = JobPhase.IDLE,
    val status: String = "Готово к работе",
    val currentFile: String = "",
    val completedBytes: Long = 0,
    val totalBytes: Long = 0,
    val logs: List<String> = emptyList(),
    val busy: Boolean = false,
)

sealed interface EngineEvent {
    data class Phase(val phase: JobPhase, val status: String) : EngineEvent
    data class Progress(
        val completedBytes: Long,
        val totalBytes: Long,
        val currentFile: String,
    ) : EngineEvent
    data class Log(val message: String) : EngineEvent
}

class JobCancelledException : Exception("Операция отменена")
class SourceReadException(message: String, cause: Throwable? = null) : Exception(message, cause)
class DestinationException(message: String, cause: Throwable? = null) : Exception(message, cause)
class FatalDestinationException(message: String, cause: Throwable? = null) : Exception(message, cause)
class HashMismatchException(message: String) : Exception(message)
class FileRetriesExhaustedException(
    val attempts: Int,
    cause: Throwable,
) : Exception("Попытки исчерпаны после $attempts запусков: ${cause.message}", cause)

class SafOperationException(
    message: String,
    cause: Throwable? = null,
    val fatalWithoutErrno: Boolean = false,
) : Exception(message, cause)

class CacheDropException(message: String, cause: Throwable? = null) : Exception(message, cause)
