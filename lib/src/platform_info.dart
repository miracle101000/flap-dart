import 'dart:io';

/// Machine architecture as reported by the OS, normalised to `x64` / `arm64`.
///
/// Uses `uname -m` on Unix-like systems (the Dart SDK offers no direct API
/// for the CPU architecture in all channels) and falls back to `x64`.
String get machineArch {
  if (Platform.isWindows) return 'x64';
  try {
    final result = Process.runSync('uname', ['-m']);
    return normaliseArch(result.stdout.toString());
  } on ProcessException {
    return 'x64';
  }
}

/// Maps raw `uname -m` output to the architecture names used in release
/// asset filenames.
String normaliseArch(String raw) {
  switch (raw.trim().toLowerCase()) {
    case 'arm64':
    case 'aarch64':
      return 'arm64';
    default:
      return 'x64';
  }
}

/// The platform slug used in GitHub release asset names, e.g. `linux-x64`.
///
/// Throws [UnsupportedError] for platforms without a pre-built binary.
String get platformSlug => platformSlugFor(Platform.operatingSystem, machineArch);

/// Pure helper behind [platformSlug] (testable without a real platform).
String platformSlugFor(String operatingSystem, String arch) {
  final supported = <String, List<String>>{
    'linux': ['x64', 'arm64'],
    'macos': ['x64', 'arm64'],
    'windows': ['x64'],
  };
  final arches = supported[operatingSystem];
  if (arches == null || !arches.contains(arch)) {
    throw UnsupportedError(
      'flap does not publish pre-built binaries for '
      '$operatingSystem/$arch.\n'
      'Build from source: https://github.com/miracle101000/flap-dart#build-from-source\n'
      'Then point FLAP_BINARY at the resulting executable.',
    );
  }
  return '$operatingSystem-$arch';
}

/// Filename of the flap binary on the current platform.
String get binaryName => Platform.isWindows ? 'flap.exe' : 'flap';

/// Archive extension used in GitHub release assets.
String get archiveExtension => Platform.isWindows ? 'zip' : 'tar.gz';
