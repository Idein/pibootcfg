import subprocess
import json
import sys

# ordered list
permissible_licenses = ['MIT']

result = subprocess.run(['cargo', 'bundle-licenses',
                        '--format', 'json'], capture_output=True, check=True)
bundle_licenses = json.loads(result.stdout)
with open('THIRD_PARTY_LIBRARY_LICENSES', mode='w') as f:
    f.write('# Thiard party library licenses\n\n')
    for lib in bundle_licenses['third_party_libraries']:
        package_name = lib['package_name']
        licenses = lib['licenses']
        if len(licenses) == 0:
            print(
                f'Error: {package_name} has no licenses', file=sys.stderr)
            sys.exit(1)
        package_license = ''
        license_text = ''
        for p in permissible_licenses:
            l = list(filter(
                lambda x: x['license'] == p, licenses))
            if l != []:
                package_license = l[0]['license']
                license_text = l[0]['text']
                break
        if package_license == '':
            print(
                f'Error: {package_name} has no permissible licenses', file=sys.stderr)
            sys.exit(1)
        f.write(f'## {package_name}\n\n')
        f.write(f'{package_license}\n\n')
        f.write(f'{license_text}\n\n\n')
