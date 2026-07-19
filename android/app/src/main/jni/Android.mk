LOCAL_PATH := $(call my-dir)

include $(CLEAR_VARS)
LOCAL_MODULE := safecopy_io
LOCAL_SRC_FILES := native_io.cpp
LOCAL_CPPFLAGS := -std=c++17 -Wall -Wextra -Werror
include $(BUILD_SHARED_LIBRARY)
