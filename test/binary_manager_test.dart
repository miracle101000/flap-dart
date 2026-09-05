import 'dart:io';

import 'package:flap/flap.dart';
import 'package:test/test.dart';

void main() {
  test('cache dir is keyed by package version', () {
    expect(BinaryManager.cacheDir(), endsWith('${Platform.pathSeparator}$packageVersion'));
  });

  test('asset URL targets the matching release tag', () {
    final url = BinaryManager.assetUrl().toString();
    expect(url, contains('/releases/download/v$packageVersion/flap-'));
    expect(url, anyOf(endsWith('.tar.gz'), endsWith('.zip')));
  });

  test('FLAP_BINARY override must exist', () async {
    final result = await Process.run(
      Platform.resolvedExecutable,
      ['run', 'bin/flap.dart', '--version'],
      environment: {flapBinaryEnv: '/definitely/not/here'},
    );
    expect(result.exitCode, 70);
    expect(result.stderr, contains('FLAP_BINARY'));
  });

  test('FLAP_BINARY override runs the given executable', () async {
    final rust = File('target/release/generate_dart');
    if (!rust.existsSync()) {
      markTestSkipped('release binary not built');
      return;
    }
    final result = await Process.run(
      Platform.resolvedExecutable,
      ['run', 'bin/flap.dart', '--version'],
      environment: {flapBinaryEnv: rust.path},
    );
    expect(result.exitCode, 0);
    expect(result.stdout, startsWith('flap $packageVersion'));
  });
}
