# Writing a job

A job is a program in any language. The cluster cuts a range of numbers into chunks, runs your program on each chunk on some PC, and joins the outputs.

## The contract

**Inputs.** Two command-line arguments: `START END`. Your program handles the numbers from START up to but not including END.

**Environment variables** (also set, for programs that prefer them):

| Variable | Meaning |
|---|---|
| `CK_JOB` | job number |
| `CK_TASK` | `<job>:<start>-<end>`, a unique name for this piece |
| `CK_START`, `CK_END` | same as the two arguments |
| `CK_ATTEMPT` | attempt number, starting at 1. Today a worker passes 1 unless its own environment sets it (the head counts attempts). |
| `CK_THREADS` | how many copies of your program run in parallel on this PC for this chunk (the PC's cores). Each copy gets its own sub-range; do not start more threads than you need. |
| `CK_SCRATCH` | an empty folder just for this run. It is deleted when your program exits. |

**Output.** Print the result to stdout. Keep it under 64 MiB per chunk; more than that fails the chunk (no retry). Print logs and errors to stderr, never to stdout: stdout is the result and is joined byte for byte.

**Exit codes.**

| Exit | Meaning |
|---|---|
| 0 | done, stdout is the result |
| 75 | try again (a temporary problem: file busy, network blip) |
| anything else, or the program cannot start | failed |

When a chunk fails, the last line of stderr (up to 300 characters) is shown with the exit code in the job's error. Make that line useful.

**Idempotent.** The same range may run twice: after a failure, or after a PC is lost mid-chunk. Your program must have no side effects outside stdout and `CK_SCRATCH`. Do not append to a shared file or send an email per chunk. The same input should give the same output.

## Retries

Exit 75 (or a timeout) puts the chunk back in the queue for another PC. Exit 75 counts as an attempt, and the head stops at **3 attempts**: after the third failed attempt the whole job is marked failed and shows the last error, so a program that always exits 75 does not loop forever. Any other failure is a failed attempt too (the head decides whether it is tried again; see docs/API.md).

## Submit

- Program already on every PC at the same path: `ck run --n 1e8 --chunk 1e6 --cmd examples/primes`
- Send a script from your PC, no copy needed elsewhere: `ck run --n 1e7 --push examples/primes.py` (the interpreter is picked from the file type; the script is limited to 64 KiB)
- Browser: Console, New job, choose range, chunk size and the program (command or uploaded script).

`ck run` prints the joined output; the per-chunk outputs are concatenated in range order. Sum them with `awk '{s+=$1} END {print s}'` if each chunk prints one number.

## Examples: count primes in [START, END)

Each prints one number. Full files are in `examples/`.

C (`cc -O2 examples/primes.c -o examples/primes`, run with `--cmd examples/primes`):

```c
#include <stdio.h>
#include <stdlib.h>
static int is_prime(unsigned long long n) {
    if (n < 2) return 0;
    if (n % 2 == 0) return n == 2;
    for (unsigned long long i = 3; i * i <= n; i += 2) if (n % i == 0) return 0;
    return 1;
}
int main(int argc, char **argv) {
    if (argc < 3) { fprintf(stderr, "usage: primes START END\n"); return 1; }
    unsigned long long s = strtoull(argv[1], 0, 10), e = strtoull(argv[2], 0, 10), c = 0;
    for (unsigned long long n = s; n < e; n++) c += is_prime(n);
    printf("%llu\n", c);
    return 0;
}
```

Python (`--push examples/primes.py`):

```python
import sys

def is_prime(n):
    if n < 2: return False
    if n % 2 == 0: return n == 2
    i = 3
    while i * i <= n:
        if n % i == 0: return False
        i += 2
    return True

start, end = int(sys.argv[1]), int(sys.argv[2])
print(sum(is_prime(n) for n in range(start, end)))
```

Node (`--push examples/primes.js`):

```js
const [start, end] = process.argv.slice(2).map(Number);
const isPrime = (n) => {
  if (n < 2) return false;
  if (n % 2 === 0) return n === 2;
  for (let i = 3; i * i <= n; i += 2) if (n % i === 0) return false;
  return true;
};
let count = 0;
for (let n = start; n < end; n++) if (isPrime(n)) count++;
console.log(count);
```

A program that needs a temporary file or a flaky resource, in Python:

```python
import os, sys
try:
    data = open("/mnt/shared/input.bin", "rb").read()      # may be busy
except OSError as e:
    print(f"input not ready: {e}", file=sys.stderr)
    sys.exit(75)                                             # retry later
work = os.path.join(os.environ["CK_SCRATCH"], "tmp.bin")    # private, deleted afterwards
```

## List jobs (one command over a list of items)

A list job runs one command over a list of items (file names, parameter sets, frame numbers) instead of a number range. Item `i` is line `i` of the list.

Submit it:

- **CLI:** `ck run --list items.txt --cmd "your-program"`.
- **Console:** New job, kind List, then paste or upload the items.

Each chunk is a range of item indexes `[a, b)`. The agent splits a chunk's items into one contiguous piece per thread. It runs the command once per piece, with that piece's items on stdin: one per line, with a trailing newline. The output of the pieces is joined in order.

The command gets no `START END` arguments. It gets the same environment as `--cmd` jobs:

- `CK_JOB`, `CK_TASK`, `CK_ATTEMPT`, `CK_THREADS`, `CK_SCRATCH`;
- `CK_START` and `CK_END`, set to the item index range of the piece.

Exit codes and limits are the same as for `--cmd` jobs: 0 ok, 75 retry, anything else failed, and at most 64 MiB of output per piece. A program that ignores stdin is fine.

Example: hash every file named in the list.

```
ck run --list files.txt --cmd "sh -c 'while read f; do sha256sum \"\$f\"; done'"
```

Combine results on the head with a reducer: `--reducer sum`, `count`, `sort`, `lines` or `top:N` (console: the Result reducer switch).

## Output files and templates (wave 15)
A `cmd` or `list` program may write result files. The agent sets env `CK_OUT` to an empty directory for the chunk. Everything the program leaves there (regular files, also in sub-directories, kept under their relative path such as `sub/a.csv`; symlinks are skipped; exit 0) is uploaded to the head when the chunk ends. The limit `out_files_mb` (default 256 MB) is checked while the program runs: past it the program is stopped and the chunk fails at once with a clear message. A program is also stopped, and its chunk failed without retry, when all its processes together use more than `job_mem_pct` (default 80, 0 = off) percent of the PC's RAM ("memory over the N MB limit"); memory is sampled about every 300 ms once the program has run 250 ms. Each program carries env `CK_RUN_ID`; processes it leaves in another process group (`setsid`, double-forked daemons) are found by that tag (about once a second), counted in the memory cap and killed on cancel and when the program ends. A program that clears its own environment escapes this. Pieces of one chunk share the directory, so name files by `$CK_START` (for example `$CK_OUT/part_$CK_START.csv`).

List them with `ck job files ID`, fetch them with `ck job files ID --get DIR`, or use `GET /api/v1/jobs/ID/files` and `GET /api/v1/blobs/SHA`.

A template stores a job body under a name: `ck job template save NAME job.json`, then `ck job submit --template NAME` (API: `POST /api/v1/jobs {"template": "NAME", "n": 5000}`; keys in the body replace the stored ones).

## Limits per chunk, lab heads, and file staging

- **Memory.** A `cmd` program and all its processes are stopped when together they use more than `job_mem_pct` (default 80) percent of the PC's RAM; the chunk fails with "memory over the N MB limit (job_mem_pct), program stopped". Not a retry: only exit 75 asks for a retry.
- **Output.** stdout beyond `out_max_mb` (64 MB) stops the program ("output over the N MiB limit"); `$CK_OUT` files beyond `out_files_mb` (256 MB) stop it too. Write large results to `$CK_OUT`, not stdout.
- **Files (blobs).** A chunk whose files have not arrived `stage_timeout_s` (120 s, plus 1 s per 2 MB) after it was reserved is re-staged from the head, then moved to another PC. If a file is gone from the head the job fails and names it.
- **Lab heads.** With lab heads on, every kind on this page runs the same way, including `$CK_OUT` files and `max_pcs`; only `--only` jobs make the root pause lab heads ([HIERARCHY.md](HIERARCHY.md)). `CK_START` and `CK_END` stay the numbers of the whole job.
- **Real examples.** `examples/real/` has five complete jobs written with the SDK (see [SDK.md](SDK.md)); `examples/real/cv.py` leaves PNG files in `$CK_OUT` as described above.
