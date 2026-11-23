#!/usr/bin/env python3
"""Execute real managed CLI/library/image catalogs; never executes Recipe units.

Build unprivileged first (see .github/workflows/ci.yml). Runtime requires root on
a disposable host whose Ginux helper, image tool, runtime and allocator are
installed at Recipe's compiled-in paths (/usr/libexec/ginux, /usr/lib64/ginux)
with the same bytes as --ginux-dir. Each case has retained logs, a complete
enumerated catalog, bounded execution and owned-process checks.
--full-products also requires explicit builder binary/recipe/repository inputs.
"""
from pathlib import Path
import argparse,atexit,concurrent.futures,hashlib,json,os,re,shutil,signal,subprocess,sys,tempfile,time
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--build-json',type=Path,required=True)
parser.add_argument('--ginux-dir',type=Path,required=True,help='the five Ginux files: ginux-cli, ginux-helper, ginux-image, ginux-runtime, libginux_allocator.so')
parser.add_argument('--ginux-source',required=True)
parser.add_argument('--image',type=Path,required=True,help='the compiler image layout tar (image.oci.tar)')
parser.add_argument('--image-digest',required=True,help='its manifest digest')
parser.add_argument('--output',type=Path,required=True)
parser.add_argument('--full-products',action='store_true')
args=parser.parse_args()
if not re.fullmatch('[0-9a-f]{40}',args.ginux_source):parser.error('Ginux source must be a complete commit SHA')
if not re.fullmatch('[0-9a-f]{64}',args.image_digest):parser.error('image must be pinned by its manifest digest')
root=Path(__file__).resolve().parent.parent
out=args.output.resolve();out.mkdir(parents=True,exist_ok=False)
def sha(p):
 with Path(p).open('rb') as f:return hashlib.file_digest(f,'sha256').hexdigest()
def sources():
 trees=[root/'src',root/'tests',root/'scripts',root/'bootstrap']
 if args.full_products:trees.append(Path(os.environ['RECIPE_TEST_BUILDER_RECIPES']))
 return {str(p):sha(p) for tree in trees for p in tree.rglob('*') if p.is_file()}
artifacts={}
for line in args.build_json.read_text().splitlines():
 message=json.loads(line)
 if message.get('reason')!='compiler-artifact' or not message.get('executable'):continue
 target=message['target'];name=target['name'];kind=target['kind']
 if name not in {'recipe','recipe-image','e2e','library_backend','image_producer'}:continue
 if kind not in [['bin'],['test']]:continue
 if name in artifacts:raise RuntimeError('duplicate executable '+name)
 path=str(Path(message['executable']).resolve())
 artifacts[name]={'path':path,'sha256':sha(path)}
if set(artifacts)!={'recipe','recipe-image','e2e','library_backend','image_producer'}:raise RuntimeError('build must contain both binaries and all three integration targets')
# The Ginux files the runs use: the CLI from --ginux-dir (RECIPE_GINUX), the
# others where the CLI and the helper find them through sudo.
GINUX={'ginux-cli':None,'ginux-helper':'/usr/libexec/ginux/ginux-helper','ginux-image':'/usr/libexec/ginux/ginux-image',
 'ginux-runtime':'/usr/libexec/ginux/ginux-runtime','libginux_allocator.so':'/usr/lib64/ginux/libginux_allocator.so'}
for name,installed in GINUX.items():
 path=(args.ginux_dir/name).resolve(strict=True);artifacts[name]={'path':str(path),'sha256':sha(path)}
 if installed is not None and sha(installed)!=artifacts[name]['sha256']:raise RuntimeError(installed+' is not the --ginux-dir '+name)
path=args.image.resolve();artifacts['image']={'path':str(path),'sha256':sha(path)}
def layout_manifest(tar):
 import tarfile
 with tarfile.open(tar) as archive:
  members={m.name.removeprefix('./'):m for m in archive.getmembers()}
  index=json.load(archive.extractfile(members['index.json']))
  if len(index['manifests'])!=1:raise RuntimeError('image layout names more than one manifest')
  digest=index['manifests'][0]['digest'].removeprefix('sha256:')
  if hashlib.sha256(archive.extractfile(members['blobs/sha256/'+digest]).read()).hexdigest()!=digest:raise RuntimeError('image manifest blob does not match its digest')
  return digest
if layout_manifest(artifacts['image']['path'])!=args.image_digest:raise RuntimeError('image manifest digest mismatch')
environment={
 'RECIPE_TEST_BIN':artifacts['recipe']['path'],
 'RECIPE_WORKER_BIN':artifacts['recipe']['path'],
 'RECIPE_GINUX':artifacts['ginux-cli']['path'],
 'RECIPE_IMAGE_LAYOUT':artifacts['image']['path'],
 'RECIPE_IMAGE':'local/recipe-compiler@sha256:'+args.image_digest,
 # The runs are root and use the fixtures' own state roots, which Recipe's
 # guard refuses (it allows only /var/lib/recipe/ginux): a root caller names
 # the installed helper itself.
 'GINUX_INSTANCE_HELPER':GINUX['ginux-helper'],
}
# A root run starts a host-side worker as an unprivileged uid, which must be able
# to execute it. A build tree under a private home is out of its reach, so the
# worker is a copy (the same bytes) in a directory every user can enter.
worker_dir=Path(tempfile.mkdtemp(prefix='recipe-worker-',dir='/var/tmp'));worker_dir.chmod(0o755)
atexit.register(shutil.rmtree,worker_dir,ignore_errors=True)
shutil.copy2(artifacts['recipe']['path'],worker_dir/'recipe');(worker_dir/'recipe').chmod(0o755)
environment['RECIPE_WORKER_BIN']=str(worker_dir/'recipe')
if args.full_products:
 for key in ['RECIPE_TEST_BUILDER','RECIPE_TEST_BUILDER_RECIPES','RECIPE_TEST_REPO_ROOT']:
  environment[key]=str(Path(os.environ[key]).resolve(strict=True))
 path=environment['RECIPE_TEST_BUILDER'];artifacts['builder']={'path':path,'sha256':sha(path)}
config={'ginux_source':args.ginux_source,'recipe_source':subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip(),
 'recipe_diff_sha256':hashlib.sha256(subprocess.check_output(['git','diff','HEAD','--'],cwd=root)).hexdigest(),
 'build_json_sha256':sha(args.build_json),'full_products':args.full_products,'artifacts':artifacts,
 'harnesses':{k:artifacts[k]['path'] for k in ['e2e','library_backend','image_producer']},'environment':environment}
(out/'combination.json').write_text(json.dumps(config,indent=2)+'\n')
for info in config['artifacts'].values():
 if sha(info['path'])!=info['sha256']:raise RuntimeError('artifact changed '+info['path'])
initial=sources()
selection=[]
for suite,harness in config['harnesses'].items():
 catalog=subprocess.check_output([harness,'--list','--format','terse'],text=True)
 (out/(suite+'-catalog.txt')).write_text(catalog)
 names=[line.removesuffix(': test') for line in catalog.splitlines() if line.endswith(': test')]
 if not names or len(set(names))!=len(names):raise RuntimeError('invalid catalog '+suite)
 ignored=subprocess.check_output([harness,'--list','--ignored','--format','terse'],text=True)
 if any(line.endswith(': test') for line in ignored.splitlines()):raise RuntimeError('ignored required test '+suite)
 selection.extend((suite,name) for name in names)
excluded=[]
if not args.full_products:
 excluded=[s for s in selection if s[0]=='e2e' and s[1].split('::')[0] in {'kernel_acquisition','kernel_recipes','preseed_recipes','target_metadata'}]
 selection=[s for s in selection if s not in excluded]
(out/'excluded-products.json').write_text(json.dumps(excluded,indent=2)+'\n')
if not selection:raise RuntimeError('empty acceptance catalog')
(out/'identity.json').write_text(json.dumps({'combination':config,'runner_sha256':sha(__file__),'source_hashes':initial,'selection':selection},indent=2)+'\n')
def identity(pid):
 try:
  parts=Path(f'/proc/{pid}/stat').read_text().rsplit(') ',1)[1].split();return parts[19],parts[0]
 except (FileNotFoundError,ProcessLookupError):return None
def descendants(pid,seen):
 token=identity(pid)
 if not token or seen.get(pid)==token[0]:return
 seen[pid]=token[0]
 try:tasks=list(Path(f'/proc/{pid}/task').iterdir())
 except (FileNotFoundError,ProcessLookupError):return
 for task in tasks:
  try:children=(task/'children').read_text().split()
  except (FileNotFoundError,ProcessLookupError):continue
  for child in children:descendants(int(child),seen)
def sample(pid,seen):
 for known,started in list(seen.items()):
  current=identity(known)
  if not current or current[0]!=started:continue
  try:tasks=list(Path(f'/proc/{known}/task').iterdir())
  except (FileNotFoundError,ProcessLookupError):continue
  for task in tasks:
   try:children=(task/'children').read_text().split()
   except (FileNotFoundError,ProcessLookupError):continue
   for child in children:descendants(int(child),seen)
 descendants(pid,seen)
def run(selected):
 suite,case=selected;work=out/(suite+'--'+case.replace('::','--'));work.mkdir()
 fixtures=work/'fixtures';fixtures.mkdir()
 env={**os.environ,**config['environment'],'RECIPE_TEST_ARTIFACTS':str(fixtures),'TMPDIR':str(work)}
 if suite=='library_backend':env.update(RECIPE_WORKER='1',RECIPE_INVOCATION='spoofed-public-call')
 else:
  env.pop('RECIPE_WORKER',None);env.pop('RECIPE_INVOCATION',None)
 command=[config['harnesses'][suite],case,'--exact','--nocapture','--test-threads=1']
 started=time.monotonic();seen={}
 receipt={'suite':suite,'case':case,'command':command,'status':'running','combination_sha256':sha(out/'combination.json'),'env':{k:env[k] for k in set(config['environment'])|{'RECIPE_TEST_ARTIFACTS','TMPDIR'}|({'RECIPE_WORKER','RECIPE_INVOCATION'} if suite=='library_backend' else set())}}
 (work/'receipt.json').write_text(json.dumps(receipt,indent=2)+'\n')
 with (work/'stdout.log').open('w') as stdout,(work/'stderr.log').open('w') as stderr:
  p=subprocess.Popen(command,cwd=root,env=env,stdout=stdout,stderr=stderr,start_new_session=True)
  while p.poll() is None and time.monotonic()-started<3600:sample(p.pid,seen);time.sleep(.1)
  timeout=p.poll() is None
  if timeout:os.killpg(p.pid,signal.SIGKILL)
  code=p.wait();deadline=time.monotonic()+5
  def alive():return {pid:token for pid,token in seen.items() if (actual:=identity(pid)) and actual[0]==token and actual[1]!='Z'}
  while alive() and time.monotonic()<deadline:time.sleep(.1)
  leaks=alive()
  for pid,token in leaks.items():
   if (actual:=identity(pid)) and actual[0]==token:os.kill(pid,signal.SIGKILL)
 text=(work/'stdout.log').read_text(errors='replace')
 status='passed' if code==0 and '1 passed; 0 failed;' in text and not leaks and not timeout else ('interrupted' if timeout else 'failed')
 scratch=[]
 for parent,dirs,files in os.walk(work,followlinks=False):
  if Path(parent).name.startswith('recipe-scratch-'):scratch.extend(str(Path(parent)/d) for d in dirs if d.startswith('invocation-'))
 receipt.update(status=status,exit_code=code,timeout=timeout,elapsed_seconds=round(time.monotonic()-started,3),tracked_processes=len(seen),live_processes_after_grace=leaks,remaining_owned_scratch=scratch,logs={name:sha(work/name) for name in ['stdout.log','stderr.log']})
 if scratch:
  receipt.update(scratch_disposition='inspect; intentional evidence not silently removed')
  if not timeout:receipt.update(status='failed')
 (work/'receipt.json').write_text(json.dumps(receipt,indent=2)+'\n')
 print(json.dumps({k:receipt[k] for k in ['suite','case','status','exit_code','elapsed_seconds','live_processes_after_grace','remaining_owned_scratch']}),flush=True)
 return receipt
with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:receipts=list(pool.map(run,selection))
(out/'results.json').write_text(json.dumps(receipts,indent=2)+'\n')
final=sources();changed={p for p in set(initial)|set(final) if initial.get(p)!=final.get(p)}
artifacts_changed=[info['path'] for info in config['artifacts'].values() if sha(info['path'])!=info['sha256']]
(out/'integrity.json').write_text(json.dumps({'changed_source_paths':sorted(changed),'changed_artifacts':artifacts_changed,'final_source_hashes':final},indent=2)+'\n')
raise SystemExit(0 if all(r['status']=='passed' for r in receipts) and not changed and not artifacts_changed else 1)
