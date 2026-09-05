import 'dart:io';

import 'binary_manager.dart';

/// Locates the `flap` native binary and runs it with [args], streaming
/// stdout/stderr directly to the parent terminal in real time.
///
/// ```dart
/// final code = await FlapRunner().run(['--out', './sdks', 'petstore.yaml']);
/// exit(code);
/// ```
class FlapRunner {
  const FlapRunner({BinaryManager? manager})
      : _manager = manager ?? const BinaryManager();

  final BinaryManager _manager;

  /// Runs `flap` with the given [args] and returns the process exit code.
  ///
  /// All output (stdout + stderr) is piped directly to the parent process so
  /// coloured terminal output, progress lines, and error messages all appear
  /// exactly as if you ran the Rust binary yourself.
  ///
  /// Binary resolution failures (unsupported platform, download errors) are
  /// reported on stderr and yield exit code 70 (`EX_SOFTWARE`) instead of an
  /// uncaught exception.
  Future<int> run(List<String> args) async {
    final String binary;
    try {
      binary = await _manager.ensureBinary();
    } on UnsupportedError catch (e) {
      stderr.writeln('[flap] ${e.message}');
      return 70;
    } on StateError catch (e) {
      stderr.writeln('[flap] ${e.message}');
      return 70;
    }

    final process = await Process.start(
      binary,
      args,
      // Inherit the parent's stdio so the Rust binary can do coloured output,
      // interactive progress, etc.
      mode: ProcessStartMode.inheritStdio,
    );
    return process.exitCode;
  }
}
