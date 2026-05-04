import 'package:flap/flap.dart';

/// Run the flap code generator programmatically.
///
/// Install the tool:
///   dart pub global activate flap
///
/// Then use it from the command line:
///   flap --out ./generated path/to/openapi.yaml
///
/// Or call [FlapRunner] directly from Dart code as shown below.
Future<void> main() async {
  final exitCode = await const FlapRunner().run([
    '--out',
    './generated',
    '--client=http',
    'api/openapi.yaml',
  ]);

  if (exitCode != 0) {
    throw Exception('flap exited with code $exitCode');
  }
}