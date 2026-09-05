import 'dart:io';

import 'package:flap/flap.dart';
import 'package:test/test.dart';

void main() {
  test('packageVersion matches pubspec.yaml', () {
    final pubspec = File('pubspec.yaml').readAsStringSync();
    final match = RegExp(r'^version:\s*(\S+)', multiLine: true).firstMatch(pubspec);
    expect(match, isNotNull);
    expect(packageVersion, match!.group(1));
  });

  test('packageVersion matches the Rust crate version', () {
    final cargo = File('Cargo.toml').readAsStringSync();
    final match = RegExp(r'^version\s*=\s*"([^"]+)"', multiLine: true).firstMatch(cargo);
    expect(match, isNotNull);
    expect(packageVersion, match!.group(1));
  });
}
