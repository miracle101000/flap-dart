import 'package:flap/src/platform_info.dart';
import 'package:test/test.dart';

void main() {
  test('normaliseArch', () {
    expect(normaliseArch('arm64\n'), 'arm64');
    expect(normaliseArch('aarch64'), 'arm64');
    expect(normaliseArch('x86_64'), 'x64');
    expect(normaliseArch(''), 'x64');
  });

  test('platformSlugFor supports the published matrix', () {
    expect(platformSlugFor('linux', 'x64'), 'linux-x64');
    expect(platformSlugFor('linux', 'arm64'), 'linux-arm64');
    expect(platformSlugFor('macos', 'arm64'), 'macos-arm64');
    expect(platformSlugFor('windows', 'x64'), 'windows-x64');
    expect(() => platformSlugFor('windows', 'arm64'), throwsUnsupportedError);
    expect(() => platformSlugFor('fuchsia', 'x64'), throwsUnsupportedError);
  });

  test('platformSlug resolves on this machine', () {
    expect(platformSlug, matches(RegExp(r'^(linux|macos|windows)-(x64|arm64)$')));
  });
}
