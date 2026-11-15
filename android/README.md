# Tresse GUI for Android

The application is the same Rust crate as on the desktop, built as a shared
object and loaded by a `NativeActivity`. The platform layer is
[gpui-mobile](https://github.com/longbridge/gpui-mobile). What else Android
needs — `android_main`, the Java classes gpui-mobile calls by name, saving a
document, the status bar, back — is
[gpui-android-glue](https://github.com/Akergez/gpui-android-glue): the crate
in `crates/tresse-gui/Cargo.toml`, its Java half as
`com.akergez.gpui:android-glue` in `app/build.gradle.kts`, at the same
version. The Java half is read from GitHub Packages, which asks for a token
even for a public package: CI passes its own; at a desk, put a personal token
with `read:packages` in `~/.gradle/gradle.properties` as `gpr.user` and
`gpr.key`, or build against a checkout of gpui-android-glue beside this
repository with `./gradlew -PglueLocal …` (and the path variant of the crate
in `Cargo.toml`). The Gradle project comes from gpui-mobile's example, under
the same licence terms.

The settings are kept in the application's private directory (`android_main`
in `crates/tresse-gui/src/lib.rs` has gpui-android-glue point the XDG variables
at it). The folders themselves are the person's, anywhere on the phone: each
is picked with the system's folder picker, and the application keeps the grant
to that one tree. The manifest declares the network permission and nothing
else: no storage permission exists to be asked for.

## Picking folders

**Add folder…** opens the Storage Access Framework's tree picker
(`app.akergez.tressegui.TreePicker`), takes a persistable read-and-write grant
on the chosen tree and remembers the URI in `settings.json`. Everything after
that goes through `app.akergez.tressegui.SafBridge`: listing a directory,
reading a byte range, writing a file, creating directories, deleting and
opening the repository lock. The Rust half, `crates/tresse-gui/src/saf.rs`,
implements the same `Vault` the desktop's paths do, so the client cannot tell a
phone's tree from a folder on a disk.

When the grant is gone — the folder was removed, or the system dropped the
permission — the folder says so and offers to be chosen again. No
`READ_EXTERNAL_STORAGE`, `WRITE_EXTERNAL_STORAGE`, or
`MANAGE_EXTERNAL_STORAGE` permission is requested.

## What is needed

- The Android SDK with a platform (`android-36`) and build tools, and an NDK
  (r27 is what `app/build.gradle.kts` names).
- `cargo install cargo-ndk`, and `rustup target add aarch64-linux-android`.
- A JDK, 17 or later.

## Building

From the repository root, the library first:

```sh
export ANDROID_HOME=$HOME/Android/Sdk
export ANDROID_NDK_HOME=$ANDROID_HOME/ndk/27.0.12077973

cargo ndk -t arm64-v8a --platform 26 -o android/app/src/main/jniLibs \
    build -p tresse-gui --lib --release
```

Build it `--release`: a debug library is very large, and the renderer's debug
labels crash the emulator's Vulkan driver.

and then the package:

```sh
cd android
./gradlew assembleDebug
adb install -r app/build/outputs/apk/debug/app-debug.apk
```

The log goes to logcat under the tag `tresse-gui`.

## A package to hand out

`build-aux/build-android.sh` does both steps with the `dist` profile and
leaves a release package in `dist/`; it is what CI runs. The package's version
is the one in the workspace's `Cargo.toml`, or the one the script is given,
which also asks for the release key: see "Signing the Android package" in the
top-level README.
