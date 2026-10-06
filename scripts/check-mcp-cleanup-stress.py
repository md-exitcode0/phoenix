"""Run the real MCP process-exit regression concurrently and preserve every result."""
import concurrent.futures
import json
from pathlib import Path
import subprocess
import sys

binary, destination = map(Path, sys.argv[1:3])
count = int(sys.argv[3]) if len(sys.argv) > 3 else 500
if not binary.is_absolute() or not destination.is_absolute() or count < 1:
    raise SystemExit('Absolute test binary and new output path, then optional positive count required')
if destination.exists():
    raise SystemExit('Refusing to overwrite existing evidence')

def run(index):
    result = subprocess.run(
        [str(binary), 'tools::mcp_client::tests::stdio_rejects_corrupt_stdout_and_nonzero_exit',
         '--exact', '--nocapture'], capture_output=True, text=True, timeout=20)
    passed = result.returncode == 0 and 'test result: ok. 1 passed;' in result.stdout
    return {'run': index, 'code': result.returncode, 'one_test_passed': passed,
            'output': '' if passed else result.stdout + result.stderr}

with concurrent.futures.ThreadPoolExecutor(max_workers=8) as executor:
    results = list(executor.map(run, range(count)))
with destination.open('x') as output:
    json.dump(results, output, indent=2)
failures = [result for result in results if not result['one_test_passed']]
print(json.dumps({'runs': count, 'failed': len(failures), 'failures': failures[:3]}, indent=2))
raise SystemExit(1 if failures else 0)
