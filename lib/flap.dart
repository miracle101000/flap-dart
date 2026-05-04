/// flap — OpenAPI → Dart/Flutter client generator.
///
/// Most users should interact with the `flap` command-line tool:
///
/// ```sh
/// dart pub global activate flap
/// flap --out ./sdks path/to/petstore.yaml
/// ```
///
/// [FlapRunner] is also exposed for programmatic use (e.g. in build scripts):
///
/// ```dart
/// import 'package:flap/flap.dart';
///
/// Future<void> main() async {
///   final code = await FlapRunner().run([
///     '--out', './generated',
///     '--client=http',
///     'api/openapi.yaml',
///   ]);
///   if (code != 0) throw Exception('flap exited with code $code');
/// }
/// ```
library;

export 'src/runner.dart' show FlapRunner;
