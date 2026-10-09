// MPI baseline for `ck run --sha`: the same job (SHA-256 of the decimal string of every number
// in [1, N], count hashes starting with PREFIX) as a classic MPI master/worker program.
// Rank 0 hands out chunks; every other rank (one per core) hashes one chunk at a time.
//   macOS: mpicc -O2 mpi_sha.c -o mpi_sha              (CommonCrypto)
//   Linux: mpicc -O2 mpi_sha.c -o mpi_sha -lcrypto     (OpenSSL; both use the CPU's SHA instructions)
//   one PC:   mpirun -np 11 --oversubscribe ./mpi_sha 1e8 1e6 000000     (10 cores + master)
//   cluster:  mpirun -np 41 --host A:11,B:10,C:10,D:10 ./mpi_sha 1e9 1e6 0000000
// Compare with: ck run --n 1e8 --sha 000000
#include <mpi.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#ifdef __APPLE__
#include <CommonCrypto/CommonDigest.h>
#define SHA256_FN(d, n, out) CC_SHA256(d, (CC_LONG)(n), out)
#else
#include <openssl/sha.h>
#define SHA256_FN(d, n, out) SHA256((const unsigned char *)(d), n, out)
#endif

typedef unsigned long long u64;
enum { TAG_WORK = 1, TAG_STOP = 2, TAG_RESULT = 3 };

static int nib[64], nlen;

static u64 hash_range(u64 a, u64 b) {
    u64 hits = 0;
    char buf[24];
    unsigned char d[32];
    for (u64 n = a; n < b; n++) {
        char *p = buf + sizeof buf;
        u64 x = n;
        do { *--p = (char)('0' + x % 10); x /= 10; } while (x);
        SHA256_FN(p, (size_t)(buf + sizeof buf - p), d);
        int ok = 1;
        for (int i = 0; i < nlen && ok; i++) ok = ((i & 1) ? (d[i / 2] & 15) : (d[i / 2] >> 4)) == nib[i];
        hits += ok;
    }
    return hits;
}

static void send_chunk(u64 *next, u64 end, u64 chunk, int to) {
    u64 r[2] = {*next, *next + chunk < end ? *next + chunk : end};
    *next = r[1];
    MPI_Send(r, 2, MPI_UNSIGNED_LONG_LONG, to, TAG_WORK, MPI_COMM_WORLD);
}

int main(int argc, char **argv) {
    MPI_Init(&argc, &argv);
    int rank, size;
    MPI_Comm_rank(MPI_COMM_WORLD, &rank);
    MPI_Comm_size(MPI_COMM_WORLD, &size);
    u64 n = argc > 1 ? (u64)strtod(argv[1], NULL) : 100000000ULL;
    u64 chunk = argc > 2 ? (u64)strtod(argv[2], NULL) : 1000000ULL;
    const char *prefix = argc > 3 ? argv[3] : "000000";
    nlen = (int)strlen(prefix);
    for (int i = 0; i < nlen && i < 64; i++) nib[i] = (int)strtol((char[]){prefix[i], 0}, NULL, 16);
    if (size < 2) { if (rank == 0) fprintf(stderr, "needs at least 2 ranks\n"); MPI_Finalize(); return 1; }

    if (rank == 0) {
        double t = MPI_Wtime();
        u64 next = 1, end = n + 1, hits = 0;
        int active = 0;
        for (int w = 1; w < size; w++) {
            if (next < end) { send_chunk(&next, end, chunk, w); active++; }
            else MPI_Send(NULL, 0, MPI_UNSIGNED_LONG_LONG, w, TAG_STOP, MPI_COMM_WORLD);
        }
        while (active) {
            u64 h;
            MPI_Status st;
            MPI_Recv(&h, 1, MPI_UNSIGNED_LONG_LONG, MPI_ANY_SOURCE, TAG_RESULT, MPI_COMM_WORLD, &st);
            hits += h;
            if (next < end) send_chunk(&next, end, chunk, st.MPI_SOURCE);
            else { MPI_Send(NULL, 0, MPI_UNSIGNED_LONG_LONG, st.MPI_SOURCE, TAG_STOP, MPI_COMM_WORLD); active--; }
        }
        double s = MPI_Wtime() - t;
        printf("MPI: %llu hashes start with '%s' in 1..%llu\n%llu numbers in %.3fs = %.1f M/s (%d ranks)\n",
               hits, prefix, n, n, s, n / s / 1e6, size);
    } else {
        for (;;) {
            u64 r[2];
            MPI_Status st;
            MPI_Recv(r, 2, MPI_UNSIGNED_LONG_LONG, 0, MPI_ANY_TAG, MPI_COMM_WORLD, &st);
            if (st.MPI_TAG == TAG_STOP) break;
            u64 h = hash_range(r[0], r[1]);
            MPI_Send(&h, 1, MPI_UNSIGNED_LONG_LONG, 0, TAG_RESULT, MPI_COMM_WORLD);
        }
    }
    MPI_Finalize();
    return 0;
}
