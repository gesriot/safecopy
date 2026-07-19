#include <jni.h>
#include <fcntl.h>

extern "C" JNIEXPORT jint JNICALL
Java_com_safecopy_android_NativeIo_nativeDropFileCache(
        JNIEnv *, jobject, jint file_descriptor) {
    return posix_fadvise(file_descriptor, 0, 0, POSIX_FADV_DONTNEED);
}
