// Rust engine `--cmd` example in C: count primes in [START, END). About 50x faster than primes.py.
//   cc -O2 examples/primes.c -o examples/primes          (on every PC; Windows: gcc or cl, gives primes.exe)
//   ./kit rs run --n 1e8 --cmd examples/primes | awk '{s+=$1} END {print s}'
#include <stdio.h>
#include <stdlib.h>

static int is_prime(unsigned long long n) {
    if (n < 2) return 0;
    if (n % 2 == 0) return n == 2;
    for (unsigned long long i = 3; i * i <= n; i += 2)
        if (n % i == 0) return 0;
    return 1;
}

int main(int argc, char **argv) {
    if (argc < 3) { fprintf(stderr, "usage: primes START END\n"); return 1; }
    unsigned long long start = strtoull(argv[1], NULL, 10), end = strtoull(argv[2], NULL, 10), count = 0;
    for (unsigned long long n = start; n < end; n++) count += is_prime(n);
    printf("%llu\n", count);
    return 0;
}
