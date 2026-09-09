"""Regression tests for rejecting stale, failed, and mismatched iOS releases."""
import json
from pathlib import Path
import plistlib
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import build_ios_release as release


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.desktop = Path(self.temp.name)
        self.apple = self.desktop / 'src-tauri/gen/apple'
        (self.apple / 'ShareExtension').mkdir(parents=True)
        (self.desktop / 'src-tauri/tauri.conf.json').write_text(json.dumps({
            'version': '0.3.0', 'bundle': {'iOS': {'bundleVersion': '3'}}}))
        self.write_info(self.apple / 'ShareExtension/Info.plist', 'ai.ax.watch-transcriber.share')
        self.archive = self.apple / 'build/desktop_iOS.xcarchive'
        self.export = self.apple / 'build/release-ipa'

    def info(self, identifier, version='0.3.0', build='3'):
        return {'CFBundleIdentifier': identifier, 'CFBundleShortVersionString': version,
                'CFBundleVersion': build}

    def write_info(self, path, identifier, **kwargs):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(plistlib.dumps(self.info(identifier, **kwargs)))

    def make_archive(self, share_build='3'):
        app = self.archive / 'Products/Applications/EchoWall.app'
        self.write_info(app / 'Info.plist', 'ai.ax.watch-transcriber')
        self.write_info(app / 'PlugIns/EchoWallShare.appex/Info.plist',
                        'ai.ax.watch-transcriber.share', build=share_build)

    def make_ipa(self, build='3'):
        self.export.mkdir(parents=True)
        with zipfile.ZipFile(self.export / 'EchoWall.ipa', 'w') as archive:
            for path, identifier in (
                ('Payload/EchoWall.app/Info.plist', 'ai.ax.watch-transcriber'),
                ('Payload/EchoWall.app/PlugIns/EchoWallShare.appex/Info.plist',
                 'ai.ax.watch-transcriber.share'),
            ):
                archive.writestr(path, plistlib.dumps(self.info(identifier, build=build)))

    def test_existing_archive_or_export_is_never_reused(self):
        for path in (self.archive, self.export):
            with self.subTest(path=path):
                path.mkdir(parents=True)
                with patch.object(release.subprocess, 'run') as run:
                    with self.assertRaisesRegex(ValueError, 'existing release output'):
                        release.build_release(self.desktop)
                    run.assert_not_called()
                path.rmdir()

    def test_build_failure_does_not_export_even_if_it_left_an_archive(self):
        def failed_build(*args, **kwargs):
            self.make_archive()
            raise subprocess.CalledProcessError(65, args[0])
        with patch.object(release.subprocess, 'run', side_effect=failed_build) as run:
            with self.assertRaises(subprocess.CalledProcessError):
                release.build_release(self.desktop)
            self.assertEqual(run.call_count, 1)
            self.assertFalse(self.export.exists())

    def test_wrong_extension_build_blocks_export(self):
        with patch.object(release.subprocess, 'run', side_effect=lambda *a, **k: self.make_archive('2')) as run:
            with self.assertRaisesRegex(ValueError, 'CFBundleVersion'):
                release.build_release(self.desktop)
            self.assertEqual(run.call_count, 1)

    def test_success_emits_only_fresh_verified_ipa(self):
        def command(argv, **kwargs):
            if argv[0] == 'npm':
                self.assertIn('--archive-only', argv)
                self.make_archive()
            elif argv[0].endswith('export_ios_archive.sh'):
                self.make_ipa()
        output = self.desktop / 'github-output'
        with patch.object(release.subprocess, 'run', side_effect=command) as run:
            with patch.dict(release.os.environ, {'GITHUB_OUTPUT': str(output)}):
                release.build_release(self.desktop)
            self.assertEqual(run.call_count, 3)
        self.assertEqual(output.read_text(), f'path={self.export / "EchoWall.ipa"}\n')

    def test_exported_ipa_version_is_checked(self):
        self.make_ipa(build='2')
        with self.assertRaisesRegex(ValueError, 'CFBundleVersion'):
            release.verify_ipa(self.export / 'EchoWall.ipa', '0.3.0', '3')

    def test_source_extension_mismatch_fails_before_build(self):
        self.write_info(self.apple / 'ShareExtension/Info.plist',
                        'ai.ax.watch-transcriber.share', build='2')
        with patch.object(release.subprocess, 'run') as run:
            with self.assertRaisesRegex(ValueError, 'does not match'):
                release.build_release(self.desktop)
            run.assert_not_called()


if __name__ == '__main__':
    unittest.main()
