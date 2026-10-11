#!/bin/bash
# Build only the ALAC decoder. No GPL/nonfree components, encoders, or network code.
set -euo pipefail
android_root=$(cd "$(dirname "$0")/.." && pwd)
vendor="$android_root/Vendor"
sdk=${ANDROID_HOME:-${ANDROID_SDK_ROOT:-$HOME/Library/Android/sdk}}
ndk="$sdk/ndk/29.0.13113456"
cmake="$sdk/cmake/3.22.1/bin/cmake"
case "$(uname -s)" in Darwin) host=darwin-x86_64 ;; Linux) host=linux-x86_64 ;; *) echo 'Build on macOS or Linux' >&2; exit 1 ;; esac
[ -d "$ndk" ] && [ -x "$cmake" ] || { echo 'Install NDK 29.0.13113456 and CMake 3.22.1 with sdkmanager' >&2; exit 1; }
mkdir -p "$vendor"
fetch() {
    local name=$1 revision=$2 checksum=$3 repository=$4
    if [ ! -f "$vendor/$name.tar.gz" ]; then curl -fL --retry 2 "https://codeload.github.com/$repository/tar.gz/$revision" -o "$vendor/$name.tar.gz"; fi
    python3 - "$vendor/$name.tar.gz" "$checksum" <<'PY'
import hashlib, sys
with open(sys.argv[1], 'rb') as source:
    digest = hashlib.sha256()
    for chunk in iter(lambda: source.read(1024 * 1024), b''):
        digest.update(chunk)
    assert digest.hexdigest() == sys.argv[2], 'Source checksum mismatch'
PY
    if [ ! -d "$vendor/$name" ]; then
        mkdir -p "$vendor/$name"
        if [ "$name" = media ]; then
            tar -xzf "$vendor/$name.tar.gz" -C "$vendor/$name" --strip-components=1 "media-$revision/libraries/decoder_ffmpeg" "media-$revision/LICENSE"
        else tar -xzf "$vendor/$name.tar.gz" -C "$vendor/$name" --strip-components=1; fi
    fi
}
fetch media dd4d94ffc48b976dee0fdf6c18669fa6edbcec2c b5d4836859bed6a3b692ceeb16d6d8dc011212c8b84760ebd4cef9f8f931924d androidx/media
fetch ffmpeg 34277e12e80031c7f89494ba543684bc1dd0be8f db715cf38d8c1757f6ed417af2be9301905a060d14fec0b1096f17bd17431ff7 FFmpeg/FFmpeg
jni="$vendor/media/libraries/decoder_ffmpeg/src/main/jni"
[ -e "$jni/ffmpeg" ] || ln -s "$vendor/ffmpeg" "$jni/ffmpeg"
toolchain="$ndk/toolchains/llvm/prebuilt/$host/bin"
jobs=${VTAMP_BUILD_JOBS:-4}
for abi in armeabi-v7a arm64-v8a x86_64; do
    [ ! -f "$vendor/jniLibs/$abi/libffmpegJNI.so" ] || continue
    case "$abi" in
        armeabi-v7a) arch=arm; triple=armv7a-linux-androideabi; cpu=armv7-a ;;
        arm64-v8a) arch=aarch64; triple=aarch64-linux-android; cpu=armv8-a ;;
        x86_64) arch=x86_64; triple=x86_64-linux-android; cpu=x86-64 ;;
    esac
    (
        cd "$vendor/ffmpeg"
        ./configure --target-os=android --arch="$arch" --cpu="$cpu" --enable-cross-compile \
            --cc="$toolchain/${triple}29-clang" --cxx="$toolchain/${triple}29-clang++" \
            --ar="$toolchain/llvm-ar" --nm="$toolchain/llvm-nm" --ranlib="$toolchain/llvm-ranlib" --strip="$toolchain/llvm-strip" \
            --libdir="android-libs/$abi" --enable-static --disable-shared --enable-pic \
            --disable-everything --disable-autodetect --disable-network --disable-programs --disable-doc \
            --disable-avdevice --disable-avformat --disable-swscale --disable-postproc --disable-avfilter \
            --disable-symver --disable-asm --enable-swresample --enable-decoder=alac
        make -j"$jobs" > "$vendor/build-$abi.log" 2>&1
        make install-libs >> "$vendor/build-$abi.log" 2>&1
    )
    "$cmake" -S "$jni" -B "$vendor/cmake-$abi" \
        -DCMAKE_TOOLCHAIN_FILE="$ndk/build/cmake/android.toolchain.cmake" \
        -DANDROID_ABI="$abi" -DANDROID_PLATFORM=android-29 -DANDROID_STL=c++_static -DCMAKE_BUILD_TYPE=Release
    "$cmake" --build "$vendor/cmake-$abi" -j"$jobs"
    mkdir -p "$vendor/jniLibs/$abi"
    cp "$vendor/cmake-$abi/libffmpegJNI.so" "$vendor/jniLibs/$abi/"
    "$toolchain/llvm-readelf" -l "$vendor/jniLibs/$abi/libffmpegJNI.so" | python3 -c 'import sys; lines=sys.stdin.read().splitlines(); loads=[x for x in lines if x.strip().startswith("LOAD")]; assert loads and all(int(x.split()[-1],16)>=16384 for x in loads), "16KB alignment required"'
    (cd "$vendor/ffmpeg" && make clean > /dev/null)
done
printf 'ALAC decoders ready in %s/jniLibs\n' "$vendor"
