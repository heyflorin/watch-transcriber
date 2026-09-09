#!/usr/bin/env python3
"""Build and export one fresh, version-checked iOS release; never reuse outputs."""
import json
import os
from pathlib import Path
import plistlib
import subprocess
import sys
import zipfile


def read_plist(path):
    with path.open('rb') as source:
        return plistlib.load(source)


def verify_info(info, identifier, version, build):
    for key, expected in (
        ('CFBundleIdentifier', identifier),
        ('CFBundleShortVersionString', version),
        ('CFBundleVersion', build),
    ):
        if info.get(key) != expected:
            raise ValueError(f'{identifier}: {key} must be {expected!r}, got {info.get(key)!r}')


def verify_archive(archive, version, build):
    apps = list((archive / 'Products/Applications').glob('*.app'))
    if len(apps) != 1:
        raise ValueError('Expected exactly one application in the fresh archive')
    app = apps[0]
    verify_info(read_plist(app / 'Info.plist'), 'ai.ax.watch-transcriber', version, build)
    verify_info(read_plist(app / 'PlugIns/EchoWallShare.appex/Info.plist'),
                'ai.ax.watch-transcriber.share', version, build)
    return app


def verify_ipa(ipa, version, build):
    with zipfile.ZipFile(ipa) as package:
        app_infos = [name for name in package.namelist()
                     if name.startswith('Payload/') and name.endswith('.app/Info.plist')
                     and name.count('/') == 2]
        if len(app_infos) != 1:
            raise ValueError('Expected exactly one application in the exported IPA')
        app_info = app_infos[0]
        share_info = app_info.removesuffix('Info.plist') + 'PlugIns/EchoWallShare.appex/Info.plist'
        verify_info(plistlib.loads(package.read(app_info)), 'ai.ax.watch-transcriber', version, build)
        verify_info(plistlib.loads(package.read(share_info)),
                    'ai.ax.watch-transcriber.share', version, build)


def build_release(desktop):
    apple = desktop / 'src-tauri/gen/apple'
    archive = apple / 'build/desktop_iOS.xcarchive'
    export = apple / 'build/release-ipa'
    for path in (archive, export):
        if path.exists():
            raise ValueError(f'Refusing to reuse existing release output: {path}')
    config = json.loads((desktop / 'src-tauri/tauri.conf.json').read_text())
    version = config['version']
    build = config['bundle']['iOS']['bundleVersion']
    share = read_plist(apple / 'ShareExtension/Info.plist')
    if share['CFBundleShortVersionString'] != version:
        raise ValueError('Share Extension version does not match tauri.conf.json')
    if share['CFBundleVersion'] != build:
        raise ValueError('Share Extension build number does not match tauri.conf.json')
    subprocess.run(['npm', 'run', 'tauri', 'ios', 'build', '--', '--archive-only',
                    '--export-method', 'app-store-connect', '--ci'], cwd=desktop, check=True)
    app = verify_archive(archive, version, build)
    subprocess.run(['/usr/bin/codesign', '--verify', '--deep', '--strict', str(app)], check=True)
    subprocess.run([str(desktop / 'scripts/export_ios_archive.sh'), str(archive),
                    str(apple / 'ExportOptions.plist'), str(export)], check=True)
    ipas = list(export.glob('*.ipa'))
    if len(ipas) != 1:
        raise ValueError('Expected exactly one IPA in the fresh export directory')
    verify_ipa(ipas[0], version, build)
    if output := os.environ.get('GITHUB_OUTPUT'):
        with open(output, 'a') as result:
            result.write(f'path={ipas[0]}\n')
    print(f'Verified iOS {version} ({build}), including the Share Extension: {ipas[0]}')


if __name__ == '__main__':
    try:
        build_release(Path(__file__).resolve().parent.parent)
    except (ValueError, OSError, KeyError, zipfile.BadZipFile, subprocess.CalledProcessError) as error:
        print(f'iOS release failed: {error}', file=sys.stderr)
        sys.exit(1)
