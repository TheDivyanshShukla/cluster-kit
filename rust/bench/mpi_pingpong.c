// MPI baseline for `ck ping`: rank 0 sends SIZE bytes to rank 1, which sends them back.
// Same stats as ck (min / median / p99 round trip).
//   mpicc -O2 mpi_pingpong.c -o mpi_pingpong
//   mpirun -np 2 ./mpi_pingpong 8 10000                                  # same PC
//   mpirun -np 2 --host HEAD_IP,WORKER_IP ./mpi_pingpong 1048576 200     # across the LAN
#include <mpi.h>
#include <stdio.h>
#include <stdlib.h>

static int cmp(const void *a, const void *b) {
    double x = *(const double *)a, y = *(const double *)b;
    return (x > y) - (x < y);
}

int main(int argc, char **argv) {
    MPI_Init(&argc, &argv);
    int rank;
    MPI_Comm_rank(MPI_COMM_WORLD, &rank);
    int size = argc > 1 ? atoi(argv[1]) : 8, count = argc > 2 ? atoi(argv[2]) : 10000;
    char *buf = calloc(size ? size : 1, 1);
    double *v = malloc(sizeof(double) * count);

    for (int i = -100; i < count; i++) {  // first 100 = warm-up, not timed
        double t = MPI_Wtime();
        if (rank == 0) {
            MPI_Send(buf, size, MPI_BYTE, 1, 0, MPI_COMM_WORLD);
            MPI_Recv(buf, size, MPI_BYTE, 1, 0, MPI_COMM_WORLD, MPI_STATUS_IGNORE);
            if (i >= 0) v[i] = (MPI_Wtime() - t) * 1e6;
        } else {
            MPI_Recv(buf, size, MPI_BYTE, 0, 0, MPI_COMM_WORLD, MPI_STATUS_IGNORE);
            MPI_Send(buf, size, MPI_BYTE, 0, 0, MPI_COMM_WORLD);
        }
    }
    if (rank == 0) {
        qsort(v, count, sizeof(double), cmp);
        double med = v[(count - 1) / 2];
        printf("MPI %d B round trip over %d pings: min %.1f us  median %.1f us  p99 %.1f us  (%.0f MB/s)\n",
               size, count, v[0], med, v[(int)((count - 1) * 0.99)], 2.0 * size / med);
    }
    MPI_Finalize();
    return 0;
}
