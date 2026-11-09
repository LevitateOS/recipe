#!/usr/bin/env python3
"""Acquire pinned public CI inputs and use the existing Ginux/Recipe producers."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--ginux-source', type=Path, required=True)
parser.add_argument('--producer', type=Path, required=True)
parser.add_argument('--input-root', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
parser.add_argument('--offline', action='store_true', help='Require already acquired pinned inputs and OCI blobs')
args = parser.parse_args()
root = Path(__file__).resolve().parent.parent
spec = json.loads((root / 'bootstrap/managed-ci.json').read_text())
manifest_path = root / 'bootstrap' / spec['image_manifest']
manifest = json.loads(manifest_path.read_text())
ginux = args.ginux_source.resolve(strict=True)
producer = args.producer.resolve(strict=True)
inputs = args.input_root.resolve()
output = args.output.absolute()
if output.exists() or output.is_symlink():
    raise SystemExit('output already exists; refuse to replace it')
if subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ginux, text=True).strip() != spec['ginux_revision']:
    raise SystemExit('Ginux checkout does not match tracked CI source pin')
if spec['version'] != 1 or manifest['version'] != 1:
    raise SystemExit('unsupported CI input or image manifest')
inputs.mkdir(parents=True, exist_ok=True)

def sha(path):
    with path.open('rb') as f:
        return hashlib.file_digest(f, 'sha256').hexdigest()

def acquire(url, destination, expected):
    if not url.startswith('https://'):
        raise SystemExit('input URL must use HTTPS')
    if destination.exists():
        if sha(destination) != expected:
            raise SystemExit('existing input digest mismatch: ' + str(destination))
        return
    if args.offline:
        raise SystemExit('required pinned input unavailable offline: ' + str(destination))
    destination.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix='.download-', dir=destination.parent)
    os.close(fd)
    temporary = Path(temporary)
    try:
        subprocess.run(['curl', '--fail', '--location', '--retry', '3', '--proto', '=https',
                        '--proto-redir', '=https', url, '--output', str(temporary)], check=True)
        if sha(temporary) != expected:
            raise SystemExit('downloaded input digest mismatch: ' + url)
        # A concurrent creator cannot make acquisition overwrite a valid input.
        os.link(temporary, destination)
    finally:
        temporary.unlink(missing_ok=True)

entries = [*manifest['rpms'], manifest['rust'], *manifest.get('rust_additions', [])]
if set(spec['sources']) != {x['path'] for x in entries}:
    raise SystemExit('CI sources must exactly match image manifest inputs')
for entry in entries:
    relative = Path(entry['path'])
    if relative.is_absolute() or '..' in relative.parts:
        raise SystemExit('invalid image input path')
    acquire(spec['sources'][entry['path']], inputs / relative, entry['sha256'])
key = inputs / 'RPM-GPG-KEY-Rocky-10'
acquire(spec['rocky_signing_key']['url'], key, spec['rocky_signing_key']['sha256'])
# A private, disposable RPM database keeps the host's trust database untouched.
with tempfile.TemporaryDirectory(prefix='recipe-ci-rpmdb-') as database:
    # Enforcing RPM domains need the normal data type even for a private DB.
    # Relabel only this owned directory; never edit host trust or policy.
    enforcing = Path('/sys/fs/selinux/enforce')
    if enforcing.exists() and enforcing.read_text().strip() == '1':
        reference = subprocess.check_output(['rpm', '--eval', '%{_dbpath}'], text=True).strip()
        subprocess.run(['chcon', '--reference=' + reference, database], check=True)
    subprocess.run(['rpm', '--dbpath', database, '--initdb'], check=True)
    subprocess.run(['rpmkeys', '--dbpath', database, '--import', str(key)], check=True)
    for entry in manifest['rpms']:
        subprocess.run(['rpmkeys', '--dbpath', database, '--checksig', str(inputs / entry['path'])], check=True)

env = dict(os.environ, GINUX_OCI_CACHE_DIR_OVERRIDE=str(inputs / 'oci-cache'),
           GINUX_OCI_WORK_DIR_OVERRIDE=str(inputs / 'oci-work'))
if not args.offline:
    subprocess.run(['./scripts/fetch-rocky-oci.sh'], cwd=ginux, env=env, check=True)
subprocess.run(['./scripts/prepare-rocky-oci.sh'], cwd=ginux, env=env, check=True)
if sha(inputs / manifest['base']['path']) != manifest['base']['sha256']:
    raise SystemExit('public OCI producer output does not match image manifest')
subprocess.run([str(producer), '--manifest', str(manifest_path), '--input-root', str(inputs),
                '--output', str(output)], check=True)
if sha(output / 'rootfs.tar') != spec['image_sha256']:
    raise SystemExit('produced compiler image digest differs from tracked CI pin')
provenance = json.loads((output / 'provenance.json').read_text())
if provenance['sha256'] != spec['image_sha256']:
    raise SystemExit('produced provenance differs from tracked CI pin')
print('managed CI image verified: ' + spec['image_sha256'])
