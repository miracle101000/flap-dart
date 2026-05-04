import 'dart:io';

/// The platform slug used in GitHub release asset names, e.g. `linux-x64`.
String get platformSlug {
  if (Platform.isLinux) return 'linux-x64';
  if (Platform.isMacOS) return _macosSlug();
  if (Platform.isWindows) return 'windows-x64';
  throw UnsupportedError(
    'flap does not publish pre-built binaries for '
    '"${Platform.operatingSystem}".\n'
    'Build from source: https://github.com/your-org/flap#prerequisites',
  );
}

String _macosSlug() {
  // `uname -m` returns `arm64` on Apple Silicon, `x86_64` on Intel.
  final result = Process.runSync('uname', ['-m']);
  final machine = result.stdout.toString().trim();
  return (machine == 'arm64' || machine == 'aarch64')
      ? 'macos-arm64'
      : 'macos-x64';
}

/// Filename of the flap binary on the current platform.
String get binaryName => Platform.isWindows ? 'flap.exe' : 'flap';

/// Archive extension used in GitHub release assets.
String get archiveExtension => Platform.isWindows ? 'zip' : 'tar.gz';
