import 'dart:io';

import 'package:archive/archive_io.dart';
import 'package:crypto/crypto.dart';
import 'package:http/http.dart' as http;
import 'package:path/path.dart' as p;

import 'platform_info.dart';

const String _packageVersion = '0.0.1';
const String _githubRepo = 'miracle101000/flap-dart';

/// Locates the pre-built `flap` native binary, downloading it from GitHub
/// Releases on first use and caching it in `~/.flap/bin/<version>/`.
///
/// The cache is keyed by package version, so upgrading via
/// `dart pub global activate flap` automatically fetches the new binary.
class BinaryManager {
  const BinaryManager();

  /// Returns the absolute path to an executable `flap` binary.
  ///
  /// If the binary is already cached the call is instant.
  /// Pass [force] to re-download (useful after a corrupt download).
  Future<String> ensureBinary({bool force = false}) async {
    final dir = _cacheDir();
    final path = p.join(dir, binaryName);

    if (!force && File(path).existsSync()) {
      return path;
    }

    await _downloadAndInstall(dir, path);
    return path;
  }

  // ── Private helpers ────────────────────────────────────────────────────────

  static String _cacheDir() {
    final home = Platform.environment['HOME'] ??
        Platform.environment['USERPROFILE'] ??
        Directory.systemTemp.path;
    return p.join(home, '.flap', 'bin', _packageVersion);
  }

  static Future<void> _downloadAndInstall(
    String destDir,
    String binaryPath,
  ) async {
    await Directory(destDir).create(recursive: true);

    final slug = platformSlug;
    final ext = archiveExtension;
    final assetName = 'flap-$slug.$ext';
    final assetUrl = Uri.parse(
      'https://github.com/$_githubRepo/releases/download/'
      'v$_packageVersion/$assetName',
    );

    stderr.writeln('[flap] Downloading binary: $assetUrl');

    final response = await http.get(assetUrl);
    if (response.statusCode != 200) {
      throw StateError(
        'Could not download flap v$_packageVersion '
        '(HTTP ${response.statusCode}).\n'
        'URL tried: $assetUrl\n\n'
        'Check that the release exists:\n'
        '  https://github.com/$_githubRepo/releases\n\n'
        'Or build from source (Rust toolchain required):\n'
        '  https://github.com/$_githubRepo#prerequisites',
      );
    }

    // Verify SHA-256 checksum if the release publishes a `.sha256` sidecar.
    await _verifyChecksum(assetUrl, response.bodyBytes);

    // Write archive to disk, extract, then clean up.
    final archivePath = p.join(destDir, assetName);
    await File(archivePath).writeAsBytes(response.bodyBytes);

    if (Platform.isWindows) {
      _extractZip(archivePath, destDir);
    } else {
      _extractTarGz(archivePath, destDir);
    }

    await File(archivePath).delete();

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
    final archive = const TarDecoder().decodeBytes(const GZipDecoder().decodeBytes(bytes));
    for (final file in archive) {
      if (!file.isFile) continue;
      final out = p.join(destDir, p.basename(file.name));
      File(out).writeAsBytesSync(file.content as List<int>);
    }
  }

  static void _extractZip(String archivePath, String destDir) {
    final bytes = File(archivePath).readAsBytesSync();
    final archive = const ZipDecoder().decodeBytes(bytes);
    for (final file in archive) {
      if (!file.isFile) continue;
      final out = p.join(destDir, p.basename(file.name));
      File(out).writeAsBytesSync(file.content as List<int>);
    }
  }

  static Future<void> _verifyChecksum(Uri assetUrl, List<int> bytes) async {
    final checksumUrl = Uri.parse('$assetUrl.sha256');
    try {
      final resp =
          await http.get(checksumUrl).timeout(const Duration(seconds: 10));
      if (resp.statusCode != 200) return; // No checksum published — skip.

      final expected = resp.body.split(RegExp(r'\s+')).first.toLowerCase();
      final actual = sha256.convert(bytes).toString();
      if (actual != expected) {
        throw StateError(
          'SHA-256 checksum mismatch for the downloaded flap binary!\n'
          '  Expected : $expected\n'
          '  Actual   : $actual\n\n'
          'The download may be corrupt. Delete ~/.flap/ and retry.',
        );
      }
      stderr.writeln('[flap] Checksum verified ✓');
    } on SocketException {
      // Network error fetching checksum — continue without verification.
    }
  }
}
