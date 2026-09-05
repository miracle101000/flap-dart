import 'dart:io';

import 'package:archive/archive_io.dart';
import 'package:crypto/crypto.dart';
import 'package:http/http.dart' as http;
import 'package:path/path.dart' as p;

import 'platform_info.dart';
import 'version.dart';

const String _githubRepo = 'miracle101000/flap-dart';

/// Environment variable that points at a locally built `flap` binary,
/// bypassing the download entirely (useful for development and CI).
const String flapBinaryEnv = 'FLAP_BINARY';

/// Locates the pre-built `flap` native binary, downloading it from GitHub
/// Releases on first use and caching it in `~/.flap/bin/<version>/`.
///
/// The cache is keyed by package version, so upgrading via
/// `dart pub global activate flap` automatically fetches the new binary.
class BinaryManager {
  const BinaryManager();

  /// Returns the absolute path to an executable `flap` binary.
  ///
  /// Resolution order:
  ///   1. `$FLAP_BINARY`, when set and pointing at an existing file.
  ///   2. The cached download for this package version.
  ///   3. A fresh download from the matching GitHub release.
  ///
  /// Pass [force] to re-download (useful after a corrupt download).
  Future<String> ensureBinary({bool force = false}) async {
    final override = Platform.environment[flapBinaryEnv];
    if (override != null && override.isNotEmpty) {
      if (!File(override).existsSync()) {
        throw StateError(
          '$flapBinaryEnv is set to "$override" but that file does not exist.',
        );
      }
      return override;
    }

    final dir = cacheDir();
    final path = p.join(dir, binaryName);

    if (!force && File(path).existsSync()) {
      return path;
    }

    await _downloadAndInstall(dir, path);
    return path;
  }

  /// `~/.flap/bin/<version>` (or the system temp dir when no home is known).
  static String cacheDir() {
    final home = Platform.environment['HOME'] ??
        Platform.environment['USERPROFILE'] ??
        Directory.systemTemp.path;
    return p.join(home, '.flap', 'bin', packageVersion);
  }

  /// URL of the release asset for the current platform.
  static Uri assetUrl() => Uri.parse(
        'https://github.com/$_githubRepo/releases/download/'
        'v$packageVersion/flap-$platformSlug.$archiveExtension',
      );

  // ── Private helpers ────────────────────────────────────────────────────────

  static Future<void> _downloadAndInstall(
    String destDir,
    String binaryPath,
  ) async {
    final url = assetUrl(); // Throws UnsupportedError before any I/O.
    await Directory(destDir).create(recursive: true);

    stderr.writeln('[flap] Downloading binary: $url');

    final http.Response response;
    try {
      response = await http.get(url);
    } on SocketException catch (e) {
      throw StateError(
        'Could not download flap v$packageVersion: $e\n'
        'Check your network connection, or build from source and set '
        '$flapBinaryEnv.',
      );
    }
    if (response.statusCode != 200) {
      throw StateError(
        'Could not download flap v$packageVersion '
        '(HTTP ${response.statusCode}).\n'
        'URL tried: $url\n\n'
        'Check that the release exists:\n'
        '  https://github.com/$_githubRepo/releases\n\n'
        'Or build from source (Rust toolchain required) and set $flapBinaryEnv:\n'
        '  https://github.com/$_githubRepo#build-from-source',
      );
    }

    // Verify SHA-256 checksum if the release publishes a `.sha256` sidecar.
    await _verifyChecksum(url, response.bodyBytes);

    // Write archive to disk, extract, then clean up.
    final archivePath = p.join(destDir, p.basename(url.path));
    await File(archivePath).writeAsBytes(response.bodyBytes);

    try {
      if (Platform.isWindows) {
        _extractZip(archivePath, destDir);
      } else {
        _extractTarGz(archivePath, destDir);
      }
    } finally {
      await File(archivePath).delete();
    }

    if (!File(binaryPath).existsSync()) {
      throw StateError(
        'Downloaded archive did not contain `$binaryName`. '
        'Delete ${cacheDir()} and retry.',
      );
    }

    // Make the binary executable on Unix-like systems.
    if (!Platform.isWindows) {
      final chmod = await Process.run('chmod', ['+x', binaryPath]);
      if (chmod.exitCode != 0) {
        throw StateError('chmod +x failed: ${chmod.stderr}');
      }
    }

    stderr.writeln('[flap] Installed to $binaryPath');
  }

  static void _extractTarGz(String archivePath, String destDir) {
    final bytes = File(archivePath).readAsBytesSync();
    final archive =
        TarDecoder().decodeBytes(const GZipDecoder().decodeBytes(bytes));
    _writeEntries(archive, destDir);
  }

  static void _extractZip(String archivePath, String destDir) {
    final bytes = File(archivePath).readAsBytesSync();
    final archive = ZipDecoder().decodeBytes(bytes);
    _writeEntries(archive, destDir);
  }

  /// Flattens the archive into [destDir] — release archives contain a single
  /// executable at the top level.
  static void _writeEntries(Archive archive, String destDir) {
    for (final file in archive) {
      if (!file.isFile) continue;
      final out = p.join(destDir, p.basename(file.name));
      File(out).writeAsBytesSync(file.content);
    }
  }

  static Future<void> _verifyChecksum(Uri assetUrl, List<int> bytes) async {
    final checksumUrl = Uri.parse('$assetUrl.sha256');
    final http.Response resp;
    try {
      resp = await http.get(checksumUrl).timeout(const Duration(seconds: 10));
    } on Exception {
      // Network error fetching the checksum — continue without verification.
      return;
    }
    if (resp.statusCode != 200) return; // No checksum published — skip.

    final expected = resp.body.trim().split(RegExp(r'\s+')).first.toLowerCase();
    final actual = sha256.convert(bytes).toString();
    if (actual != expected) {
      throw StateError(
        'SHA-256 checksum mismatch for the downloaded flap binary!\n'
        '  Expected : $expected\n'
        '  Actual   : $actual\n\n'
        'The download may be corrupt. Delete ${cacheDir()} and retry.',
      );
    }
    stderr.writeln('[flap] Checksum verified ✓');
  }
}
