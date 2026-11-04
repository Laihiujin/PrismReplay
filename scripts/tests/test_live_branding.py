import sys,json,pathlib,hashlib,struct,re,subprocess
root=pathlib.Path(sys.argv[1]) if len(sys.argv)>1 else pathlib.Path(__file__).resolve().parents[2]; failures=[]
def check(name,ok):
 print(f'{name}: {"PASS" if ok else "FAIL"}',flush=True)
 if not ok:failures.append(name)
base=json.loads((root/'src-tauri/tauri.conf.json').read_text())
print('productName='+base['productName'],flush=True)
print('package.name='+json.loads((root/'package.json').read_text())['name'],flush=True)
check('product and binary are LIVE',base['productName']=='LIVE' and base.get('mainBinaryName')=='LIVE')
check('package is LIVE',json.loads((root/'package.json').read_text())['name']=='LIVE')
check('Rust package is live','name = "live"' in (root/'src-tauri/Cargo.toml').read_text())
for p in sorted((root/'src-tauri').glob('tauri.*.conf.json')):
 c=json.loads(p.read_text());check(p.name+' product',c.get('productName','LIVE')=='LIVE')
 for w in c.get('app',{}).get('windows',[]):check(p.name+' window title',w.get('title')=='LIVE')
for name in ['index.html','index_live.html','index_clip.html']:
 s=(root/name).read_text();check(name+' title and favicon','<title>LIVE</title>' in s and '/app-icon.png' in s and '/vite.svg' not in s)
source=root/'assets/branding/LIVE.jpg'
check('original icon hash',source.exists() and hashlib.sha256(source.read_bytes()).hexdigest()=='0bc23fa32e48930bdefab415e9895bbd9555a50cc5428815e648b6b932baacc2')
check('UI icon wired','src="/app-icon.png"' in (root/'src/page/About.svelte').read_text())
for p in [root/'public/app-icon.png',root/'src-tauri/icons/icon.png',root/'docs/public/images/icon.png']:
 check(str(p.relative_to(root)),p.exists() and p.read_bytes().startswith(b'\x89PNG\r\n\x1a\n'))
check('installer names', 'APP_NAME:-LIVE' in (root/'scripts/package-macos-dmg.sh').read_text() and 'LIVE.app/Contents/MacOS/LIVE' in (root/'.github/workflows/build-dmg.yml').read_text())
# Active identity/storage keys use LIVE; only migration inputs and real repository URLs retain legacy names.
check('LIVE data identity',base['identifier']=='cn.live' and '"cn.live"' in (root/'src-tauri/src/constants.rs').read_text())
check('LIVE deep links',base['plugins']['deep-link']['desktop']['schemes']==['live'])
print('failures='+str(len(failures)),flush=True)
sys.exit(bool(failures))
