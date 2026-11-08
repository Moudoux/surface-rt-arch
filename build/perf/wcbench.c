/* Copy bandwidth into a tegra GEM BO mapping (write-combined) vs malloc
 * memory: glibc memcpy, NEON 64-byte copies, memset. Size = one full-screen
 * BGRA texture (1366x768x4). Unprivileged: opens /dev/dri/renderD128.
 *   wcbench [iterations] */
#include <arm_neon.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

struct gem_create { uint64_t size; uint32_t flags, handle; };
struct gem_mmap { uint32_t handle, pad; uint64_t offset; };
#define DRM_IOCTL_BASE 'd'
#define DRM_COMMAND_BASE 0x40
#define IOCTL_GEM_CREATE _IOWR(DRM_IOCTL_BASE, DRM_COMMAND_BASE + 0, struct gem_create)
#define IOCTL_GEM_MMAP _IOWR(DRM_IOCTL_BASE, DRM_COMMAND_BASE + 1, struct gem_mmap)

#define W 1366
#define H 768
#define SIZE (W * H * 4)

static double now(void)
{
   struct timespec ts;
   clock_gettime(CLOCK_MONOTONIC, &ts);
   return ts.tv_sec + ts.tv_nsec * 1e-9;
}

static void neon_copy64(void *dst, const void *src, size_t n)
{
   uint8_t *d = dst;
   const uint8_t *s = src;
   for (; n >= 64; n -= 64, d += 64, s += 64) {
      uint8x16x4_t v = vld1q_u8_x4(s);
      vst1q_u8_x4(d, v);
   }
   memcpy(d, s, n);
}

/* NEON copy with source prefetch a few cache lines ahead */
static void neon_copy64_pld(void *dst, const void *src, size_t n)
{
   uint8_t *d = dst;
   const uint8_t *s = src;
   for (; n >= 64; n -= 64, d += 64, s += 64) {
      __builtin_prefetch(s + 256);
      uint8x16x4_t v = vld1q_u8_x4(s);
      vst1q_u8_x4(d, v);
   }
   memcpy(d, s, n);
}

static void bench(const char *name, int iters, void (*fn)(void *, const void *, size_t),
                  void *dst, const void *src)
{
   fn(dst, src, SIZE); /* warm-up (page faults) */
   double best = 1e9, sum = 0;
   for (int i = 0; i < iters; i++) {
      double t = now();
      fn(dst, src, SIZE);
      t = now() - t;
      sum += t;
      if (t < best) best = t;
   }
   printf("%-34s mean %6.2f ms  best %6.2f ms  %5.0f MB/s\n", name, sum / iters * 1e3,
          best * 1e3, SIZE / (sum / iters) / 1e6);
}

static void do_memcpy(void *d, const void *s, size_t n) { memcpy(d, s, n); }
static void do_memset(void *d, const void *s, size_t n) { (void)s; memset(d, 0x5a, n); }
static void do_read(void *d, const void *s, size_t n)
{
   const uint32_t *p = s;
   uint32_t acc = 0;
   for (size_t i = 0; i < n / 4; i += 8) acc += p[i];
   *(volatile uint32_t *)d = acc;
}

int main(int argc, char **argv)
{
   int iters = argc > 1 ? atoi(argv[1]) : 20;
   int fd = open("/dev/dri/renderD128", O_RDWR);
   if (fd < 0) { perror("renderD128"); return 1; }
   struct gem_create c = {.size = SIZE};
   if (ioctl(fd, IOCTL_GEM_CREATE, &c)) { perror("GEM_CREATE"); return 1; }
   struct gem_mmap m = {.handle = c.handle};
   if (ioctl(fd, IOCTL_GEM_MMAP, &m)) { perror("GEM_MMAP"); return 1; }
   void *bo = mmap(NULL, SIZE, PROT_READ | PROT_WRITE, MAP_SHARED, fd, m.offset);
   if (bo == MAP_FAILED) { perror("mmap"); return 1; }
   void *src, *dst;
   if (posix_memalign(&src, 64, SIZE) || posix_memalign(&dst, 64, SIZE)) return 1;
   memset(src, 0x33, SIZE);
   memset(dst, 0, SIZE);

   bench("memcpy malloc -> malloc", iters, do_memcpy, dst, src);
   bench("memcpy malloc -> BO", iters, do_memcpy, bo, src);
   bench("neon64 malloc -> malloc", iters, neon_copy64, dst, src);
   bench("neon64 malloc -> BO", iters, neon_copy64, bo, src);
   bench("neon64+pld malloc -> BO", iters, neon_copy64_pld, bo, src);
   bench("memset BO (write only)", iters, do_memset, bo, NULL);
   bench("memset malloc (write only)", iters, do_memset, dst, NULL);
   bench("read malloc (1 word/line)", iters, do_read, dst, src);
   return 0;
}
