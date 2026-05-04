import 'dart:io';

import 'package:flap/src/runner.dart';

/// Entry point for the `flap` global command.
///
/// Install:
///   dart pub global activate flap
///
/// Usage:
///   flap --out ./sdks path/to/petstore.yaml
///   flap --out ./sdks --client=http tests/fixtures/petstore.yaml
///   flap --help
Future<void> main(List<String> args) async {
  exitCode = await FlapRunner().run(args);
}
