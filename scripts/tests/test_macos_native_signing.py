"""Exercise the actual build entry and DMG packager using real tiny Mach-O executables.

Only Rust compilation is substituted: the yarn shim invokes the installed Tauri
bundler against a clang-built executable. Signing, disk image creation, mounting,
version checks and architecture validation use the native macOS commands.
"""
from pathlib import Path
import subprocess,tempfile,os,sys,json,shutil,shlex,argparse
parser=argparse.ArgumentParser();parser.add_argument('source',type=Path,nargs='?',default=Path(__file__).resolve().parents[2]);parser.add_argument('--target',default='aarch64-apple-darwin');args=parser.parse_args();source=args.source.resolve()
cli=(source/'node_modules/.bin/tauri').resolve();target=args.target
architectures={'aarch64-apple-darwin':['arm64'],'x86_64-apple-darwin':['x86_64'],'universal-apple-darwin':['arm64','x86_64']}[target]
sdk=subprocess.check_output(['xcrun','--sdk','macosx','--show-sdk-path'],text=True).strip()
list_before=subprocess.check_output(['security','list-keychains','-d','user'],text=True)
default_before=subprocess.check_output(['security','default-keychain','-d','user'],text=True)
try:
 with tempfile.TemporaryDirectory(prefix='live-native-signing-') as tmp:
  root=Path(tmp);(root/'scripts').mkdir();(root/'src-tauri/src').mkdir(parents=True);(root/'dist').mkdir();(root/'bin').mkdir();(root/'dist/index.html').write_text('<title>LIVE signing fixture</title>')
  for name in ['build-macos-dmg.sh','package-macos-dmg.sh','macos-portable.cmake']:shutil.copyfile(source/'scripts'/name,root/'scripts'/name)
  (root/'package.json').write_text(json.dumps({'name':'LIVE','version':'3.19.11','private':True}))
  (root/'src-tauri/Cargo.toml').write_text('[package]\nname="live"\nversion="3.19.11"\nedition="2021"\n')
  (root/'src-tauri/src/main.rs').write_text('fn main() {}\n')
  (root/'src-tauri/tauri.conf.json').write_text(json.dumps({'productName':'LIVE','mainBinaryName':'LIVE','version':'3.19.11','identifier':'cn.live.signingfixture','build':{'frontendDist':'../dist'},'bundle':{'active':True,'targets':['app'],'icon':[],'macOS':{'minimumSystemVersion':'13.3'}}}))
  output=root/'src-tauri/target'/target/'release';output.mkdir(parents=True)
  (root/'main.c').write_text('int main(void) { return 0; }\n')
  binaries=[]
  for arch in architectures:
   binary=root/('native-'+arch);subprocess.run(['xcrun','clang','-target',arch+'-apple-macos13.3','-isysroot',sdk,str(root/'main.c'),'-o',str(binary)],check=True);binaries.append(binary)
  if len(binaries)==1:shutil.copyfile(binaries[0],output/'LIVE')
  else:subprocess.run(['lipo','-create',*[str(p) for p in binaries],'-output',str(output/'LIVE')],check=True)
  (output/'LIVE').chmod(0o755)
  shim=root/'bin/yarn';shim.write_text('#!/bin/bash\nset -eu\n[[ "$1" == tauri && "$2" == build ]]\nshift 2\nexec "$NATIVE_TAURI_CLI" bundle --ci "$@"\n');shim.chmod(0o755)
  env=dict(os.environ,PATH=str(root/'bin')+os.pathsep+os.environ['PATH'],NATIVE_TAURI_CLI=str(cli),SDKROOT=sdk,LIBCLANG_PATH='/Library/Developer/CommandLineTools/usr/lib',APPLE_CERTIFICATE='',APPLE_CERTIFICATE_PASSWORD='',APPLE_SIGNING_IDENTITY='-',APPLE_ID='',APPLE_PASSWORD='',APPLE_TEAM_ID='')
  p=subprocess.run(['bash',str(root/'scripts/build-macos-dmg.sh'),target],cwd=root,env=env,capture_output=True,text=True)
  print(p.stdout,end='');print(p.stderr,end='',file=sys.stderr)
  if p.returncode:sys.exit(p.returncode)
  suffix={'aarch64-apple-darwin':'aarch64','x86_64-apple-darwin':'x86_64','universal-apple-darwin':'universal'}[target]
  dmg=output/'bundle/dmg'/f'LIVE_3.19.11_{suffix}.dmg';assert dmg.is_file()
  subprocess.run(['codesign','--verify','--deep','--strict',str(output/'bundle/macos/LIVE.app')],check=True)
  for arch in architectures:
   subprocess.run(['lipo',str(output/'bundle/macos/LIVE.app/Contents/MacOS/LIVE'),'-verify_arch',arch],check=True)
  print(f'NATIVE SIGNING PASS: {target}; real Mach-O, Tauri app bundle, ad-hoc signature, verified mounted DMG')
finally:
 after=subprocess.check_output(['security','list-keychains','-d','user'],text=True)
 if after!=list_before:subprocess.run(['security','list-keychains','-d','user','-s',*shlex.split(list_before)],check=True)
 after_default=subprocess.check_output(['security','default-keychain','-d','user'],text=True)
 if after_default!=default_before:subprocess.run(['security','default-keychain','-d','user','-s',*shlex.split(default_before)],check=True)
 assert subprocess.check_output(['security','list-keychains','-d','user'],text=True)==list_before
 print('Native test keychain search list/default restored unchanged',flush=True)
