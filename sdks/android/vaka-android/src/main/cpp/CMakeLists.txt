cmake_minimum_required(VERSION 3.18.1)
project(vaka C)

add_library(vaka SHARED vaka_crash.c vaka_jni.c)
target_compile_options(vaka PRIVATE -Wall -Wextra -Werror -O2 -fvisibility=hidden)
# 16 KB page alignment (required for Android 15+ devices with 16 KB pages).
target_link_options(vaka PRIVATE "-Wl,-z,max-page-size=16384")
