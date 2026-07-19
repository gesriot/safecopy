package com.safecopy.android

import android.system.ErrnoException
import android.system.Os

object NativeIo {
    private val loadFailure = runCatching {
        System.loadLibrary("safecopy_io")
    }.exceptionOrNull()

    private external fun nativeDropFileCache(fileDescriptor: Int): Int

    fun requireAvailable() {
        if (loadFailure != null) {
            throw CacheDropException(
                "Нативный модуль сброса файлового кэша не загрузился",
                loadFailure,
            )
        }
    }

    fun dropFileCache(fileDescriptor: Int) {
        requireAvailable()
        val errno = try {
            nativeDropFileCache(fileDescriptor)
        } catch (error: LinkageError) {
            throw CacheDropException("JNI-символ posix_fadvise недоступен", error)
        } catch (error: Exception) {
            throw CacheDropException("Вызов posix_fadvise(DONTNEED) завершился ошибкой", error)
        }
        if (errno != 0) {
            throw CacheDropException(
                "posix_fadvise(DONTNEED): ${Os.strerror(errno)}",
                ErrnoException("posix_fadvise", errno),
            )
        }
    }
}
