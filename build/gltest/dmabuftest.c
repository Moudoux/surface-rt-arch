/* dmabuf render-target test for grate (what Chromium's Ozone/Wayland does):
 * allocate a GBM bo per DRM format on the render node, import it with
 * EGL_EXT_image_dma_buf_import, bind it as GL_TEXTURE_2D via
 * glEGLImageTargetTexture2DOES, attach it to an FBO, print
 * glCheckFramebufferStatus. With "draw" also clear to (r,g,b,a) =
 * (0.8,0.4,0.2,1.0) via a shader quad, then read the bo's first pixel through a
 * CPU map: the byte order shows whether the hardware stored the channels right.
 *   dmabuftest [draw]
 *   dmabuftest full [W H [FOURCC]]   full-size producer/consumer check, see full_test()
 * Build: build/gltest/build.sh
 */
#define GL_GLEXT_PROTOTYPES 1
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <GLES2/gl2ext.h>
#include <drm_fourcc.h>
#include <fcntl.h>
#include <gbm.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static GLuint prog(void)
{
   const char *vs = "attribute vec2 p; void main() { gl_Position = vec4(p, 0.0, 1.0); }";
   const char *fs = "precision mediump float; void main() { gl_FragColor = vec4(0.8, 0.4, 0.2, 1.0); }";
   GLuint p = glCreateProgram();
   GLuint v = glCreateShader(GL_VERTEX_SHADER), f = glCreateShader(GL_FRAGMENT_SHADER);
   glShaderSource(v, 1, &vs, NULL); glCompileShader(v);
   glShaderSource(f, 1, &fs, NULL); glCompileShader(f);
   glAttachShader(p, v); glAttachShader(p, f);
   glBindAttribLocation(p, 0, "p");
   glLinkProgram(p);
   return p;
}

static PFNEGLGETPLATFORMDISPLAYEXTPROC get_display;
static PFNEGLCREATEIMAGEKHRPROC create_image;
static PFNEGLDESTROYIMAGEKHRPROC destroy_image;
static PFNGLEGLIMAGETARGETTEXTURE2DOESPROC image_target;

/* ---- "full" mode: what Chromium + rtshell do with a whole window buffer ----
 * GBM bo W x H (default 1366x768, ABGR8888 like Chromium's), imported as a render target, a pattern
 * with a unique value per pixel drawn into it and read back (producer check); then imported a second
 * time like a compositor does, sampled 1:1 onto a W x H offscreen texture with GL_REPEAT and with
 * GL_CLAMP_TO_EDGE (NEAREST), read back, compared pixel by pixel with the producer's read-back. Also
 * the same comparison for an ordinary glTexImage2D texture of the same size (not a dmabuf). Prints
 * the first mismatch and the x/y extent of the mismatching pixels (e.g. "x>=1024" = wrap at 1024). */
static GLuint build(const char *vs, const char *fs)
{
   GLuint p = glCreateProgram();
   GLuint v = glCreateShader(GL_VERTEX_SHADER), f = glCreateShader(GL_FRAGMENT_SHADER);
   glShaderSource(v, 1, &vs, NULL); glCompileShader(v);
   glShaderSource(f, 1, &fs, NULL); glCompileShader(f);
   GLint ok = 0;
   glGetShaderiv(f, GL_COMPILE_STATUS, &ok);
   if (!ok) { char log[512]; glGetShaderInfoLog(f, sizeof log, NULL, log); fprintf(stderr, "fs: %s\n", log); }
   glAttachShader(p, v); glAttachShader(p, f);
   glBindAttribLocation(p, 0, "p");
   glLinkProgram(p);
   return p;
}

static const float quad[] = { -1, -1, 1, -1, -1, 1, 1, 1 };

static void draw_quad(GLuint p, int w, int h)
{
   glViewport(0, 0, w, h);
   glUseProgram(p);
   glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, quad);
   glEnableVertexAttribArray(0);
   glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
}

static GLuint fbo_for(GLuint tex)
{
   GLuint fbo;
   glGenFramebuffers(1, &fbo);
   glBindFramebuffer(GL_FRAMEBUFFER, fbo);
   glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);
   return fbo;
}

/* Sample `src` 1:1 into a fresh W x H texture with the given wrap mode, read back into out. */
static int sample_to(GLuint prog, GLuint src, GLenum wrap, int w, int h, unsigned char *out)
{
   glBindTexture(GL_TEXTURE_2D, src);
   glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, wrap);
   glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, wrap);
   glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
   glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
   GLuint dst;
   glGenTextures(1, &dst);
   glBindTexture(GL_TEXTURE_2D, dst);
   glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, w, h, 0, GL_RGBA, GL_UNSIGNED_BYTE, NULL);
   GLuint fbo = fbo_for(dst);
   GLenum st = glCheckFramebufferStatus(GL_FRAMEBUFFER);
   if (st != GL_FRAMEBUFFER_COMPLETE) { printf("  dst fbo status 0x%x\n", st); return -1; }
   glClearColor(1, 0, 1, 1);
   glClear(GL_COLOR_BUFFER_BIT);
   glActiveTexture(GL_TEXTURE0);
   glBindTexture(GL_TEXTURE_2D, src);
   glUseProgram(prog);
   glUniform1i(glGetUniformLocation(prog, "t"), 0);
   draw_quad(prog, w, h);
   glReadPixels(0, 0, w, h, GL_RGBA, GL_UNSIGNED_BYTE, out);
   glBindFramebuffer(GL_FRAMEBUFFER, 0);
   glDeleteFramebuffers(1, &fbo);
   glDeleteTextures(1, &dst);
   return 0;
}

/* Compare RGB (alpha may differ for X formats); report count, first mismatch and extent. */
static int compare(const char *what, const unsigned char *a, const unsigned char *b, int w, int h)
{
   long bad = 0; int x0 = w, y0 = h, x1 = -1, y1 = -1, fx = -1, fy = -1;
   for (int y = 0; y < h; y++)
      for (int x = 0; x < w; x++) {
         const unsigned char *p = a + 4 * (y * w + x), *q = b + 4 * (y * w + x);
         if (abs(p[0] - q[0]) > 1 || abs(p[1] - q[1]) > 1 || abs(p[2] - q[2]) > 1) {
            if (!bad) { fx = x; fy = y; }
            bad++;
            if (x < x0) x0 = x;
            if (y < y0) y0 = y;
            if (x > x1) x1 = x;
            if (y > y1) y1 = y;
         }
      }
   if (!bad) {
      printf("%-44s OK\n", what);
      return 0;
   }
   const unsigned char *p = a + 4 * (fy * w + fx), *q = b + 4 * (fy * w + fx);
   printf("%-44s FAIL %ld px, x %d..%d y %d..%d; first (%d,%d) got %d,%d,%d want %d,%d,%d\n", what, bad,
          x0, x1, y0, y1, fx, fy, q[0], q[1], q[2], p[0], p[1], p[2]);
   return 1;
}

static int full_test(struct gbm_device *gbm, EGLDisplay dpy, int w, int h, uint32_t fourcc, uint32_t gbm_flags)
{
   /* pattern (CPU): r,g: low 8 bits of x and y; b: high bits of x (3) and y (3) + 1 marker bit: unique per pixel up to 2048x2048. */
   const char *vs = "attribute vec2 p; varying vec2 uv; void main() { uv = p * 0.5 + 0.5; gl_Position = vec4(p, 0.0, 1.0); }";
   const char *fs_tex = "precision mediump float; varying vec2 uv; uniform sampler2D t; void main() { gl_FragColor = texture2D(t, uv); }";
   GLuint smp = build(vs, fs_tex);
   int fails = 0;

   struct gbm_bo *bo = gbm_bo_create(gbm, w, h, fourcc, gbm_flags);
   if (!bo) { printf("gbm_bo_create %dx%d flags 0x%x failed\n", w, h, gbm_flags); return 1; }
   uint32_t stride = gbm_bo_get_stride(bo);
   uint64_t mod = gbm_bo_get_modifier(bo);
   printf("bo %dx%d fourcc %.4s flags 0x%x stride %u (tight %d) mod 0x%llx\n", w, h, (char *)&fourcc, gbm_flags,
          stride, w * 4, (unsigned long long)mod);
   EGLImageKHR img[2];
   for (int i = 0; i < 2; i++) {
      int dfd = gbm_bo_get_fd(bo);
      EGLint attr[] = {
         EGL_WIDTH, w, EGL_HEIGHT, h, EGL_LINUX_DRM_FOURCC_EXT, (EGLint)fourcc,
         EGL_DMA_BUF_PLANE0_FD_EXT, dfd, EGL_DMA_BUF_PLANE0_OFFSET_EXT, 0,
         EGL_DMA_BUF_PLANE0_PITCH_EXT, (EGLint)stride, EGL_NONE };
      img[i] = create_image(dpy, EGL_NO_CONTEXT, EGL_LINUX_DMA_BUF_EXT, NULL, attr);
      close(dfd);
      if (img[i] == EGL_NO_IMAGE_KHR) { printf("eglCreateImage %d failed 0x%x\n", i, eglGetError()); return 1; }
   }
   unsigned char *want = calloc(w * h, 4), *got = calloc(w * h, 4);

   /* Expected pattern, computed on the CPU. (Computing it in a fragment shader is not exact on Tegra's
    * fragment precision, so the producer draws it from a plain texture with CLAMP_TO_EDGE + NEAREST,
    * which is checked separately below.) */
   for (int y = 0; y < h; y++)
      for (int x = 0; x < w; x++) {
         unsigned char *p = want + 4 * (y * w + x);
         p[0] = x & 255; p[1] = y & 255; p[2] = (x >> 8) * 8 + (y >> 8) + 128; p[3] = 255;
      }
   GLuint plain;
   glGenTextures(1, &plain);
   glBindTexture(GL_TEXTURE_2D, plain);
   glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, w, h, 0, GL_RGBA, GL_UNSIGNED_BYTE, want);
   glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
   glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
   glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
   glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);

   /* producer: draw the pattern into the bo (import 0) like a client renders its window, read it back */
   GLuint rt;
   glGenTextures(1, &rt);
   glBindTexture(GL_TEXTURE_2D, rt);
   image_target(GL_TEXTURE_2D, img[0]);
   GLuint fbo = fbo_for(rt);
   GLenum st = glCheckFramebufferStatus(GL_FRAMEBUFFER);
   if (st != GL_FRAMEBUFFER_COMPLETE) { printf("bo fbo status 0x%x\n", st); return 1; }
   glActiveTexture(GL_TEXTURE0);
   glBindTexture(GL_TEXTURE_2D, plain);
   glUseProgram(smp);
   glUniform1i(glGetUniformLocation(smp, "t"), 0);
   draw_quad(smp, w, h);
   glReadPixels(0, 0, w, h, GL_RGBA, GL_UNSIGNED_BYTE, got);
   glFinish();
   fails += compare("producer: pattern -> bo, glReadPixels", want, got, w, h);
   glBindFramebuffer(GL_FRAMEBUFFER, 0);
   glDeleteFramebuffers(1, &fbo);
   /* and through a CPU map of the bo (what another process sees; row order as stored) */
   {
      uint32_t mstride; void *map_data = NULL;
      unsigned char *m = gbm_bo_map(bo, 0, 0, w, h, GBM_BO_TRANSFER_READ, &mstride, &map_data);
      if (m) {
         for (int y = 0; y < h; y++)
            memcpy(got + 4 * w * y, m + (size_t)mstride * y, 4 * w);
         gbm_bo_unmap(bo, map_data);
         fails += compare("producer: bo via gbm_bo_map (CPU)", want, got, w, h);
      } else
         printf("gbm_bo_map failed\n");
   }

   /* consumer: second import, sampled like a compositor */
   GLuint tex;
   glGenTextures(1, &tex);
   glBindTexture(GL_TEXTURE_2D, tex);
   image_target(GL_TEXTURE_2D, img[1]);
   static const struct { GLenum wrap; const char *name; } wraps[] = {
      { GL_CLAMP_TO_EDGE, "CLAMP_TO_EDGE" }, { GL_REPEAT, "REPEAT (GL default)" } };
   char what[64];
   for (int i = 0; i < 2; i++) {
      if (sample_to(smp, tex, wraps[i].wrap, w, h, got) == 0) {
         snprintf(what, sizeof what, "consumer: dmabuf import, %s", wraps[i].name);
         fails += compare(what, want, got, w, h);
      }
   }
   /* the same with an ordinary NPOT texture (glTexImage2D of the expected pattern) */
   for (int i = 0; i < 2; i++) {
      if (sample_to(smp, plain, wraps[i].wrap, w, h, got) == 0) {
         snprintf(what, sizeof what, "plain glTexImage2D texture, %s", wraps[i].name);
         fails += compare(what, want, got, w, h);
      }
   }
   glDeleteTextures(1, &plain);
   glDeleteTextures(1, &tex);
   glDeleteTextures(1, &rt);
   destroy_image(dpy, img[0]);
   destroy_image(dpy, img[1]);
   gbm_bo_destroy(bo);
   free(want); free(got);
   return fails;
}

int main(int argc, char **argv)
{
   get_display = (void *)eglGetProcAddress("eglGetPlatformDisplayEXT");
   create_image = (void *)eglGetProcAddress("eglCreateImageKHR");
   destroy_image = (void *)eglGetProcAddress("eglDestroyImageKHR");
   image_target = (void *)eglGetProcAddress("glEGLImageTargetTexture2DOES");
   int draw = argc > 1 && !strcmp(argv[1], "draw");
   int full = argc > 1 && !strcmp(argv[1], "full");
   int fd = open("/dev/dri/renderD128", O_RDWR);
   if (fd < 0) { perror("renderD128"); return 1; }
   struct gbm_device *gbm = gbm_create_device(fd);
   EGLDisplay dpy = get_display(EGL_PLATFORM_GBM_KHR, gbm, NULL);
   if (!eglInitialize(dpy, NULL, NULL)) { fprintf(stderr, "eglInitialize failed\n"); return 1; }
   eglBindAPI(EGL_OPENGL_ES_API);
   static const EGLint es_attr[] = { EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE };
   EGLContext ctx = eglCreateContext(dpy, EGL_NO_CONFIG_KHR, EGL_NO_CONTEXT, es_attr);
   if (ctx == EGL_NO_CONTEXT || !eglMakeCurrent(dpy, EGL_NO_SURFACE, EGL_NO_SURFACE, ctx)) {
      fprintf(stderr, "context failed: 0x%x\n", eglGetError());
      return 1;
   }
   printf("GL_RENDERER %s\n", glGetString(GL_RENDERER));
   if (full) {
      /* dmabuftest full [W H [fourcc-name]]: e.g. full 1366 768 AB24; GBM flags as Chromium's GBM buffers */
      int w = argc > 3 ? atoi(argv[2]) : 1366, h = argc > 3 ? atoi(argv[3]) : 768;
      uint32_t fourcc = DRM_FORMAT_ABGR8888;
      if (argc > 4 && strlen(argv[4]) == 4) memcpy(&fourcc, argv[4], 4);
      int fails = full_test(gbm, dpy, w, h, fourcc, GBM_BO_USE_RENDERING | GBM_BO_USE_SCANOUT);
      fails += full_test(gbm, dpy, w, h, fourcc, GBM_BO_USE_RENDERING);
      printf("%s\n", fails ? "FAILURES" : "ALL OK");
      return fails != 0;
   }
   GLuint p = draw ? prog() : 0;
   static const struct { const char *name; uint32_t fourcc; } fmts[] = {
      { "ARGB8888 (BGRA bytes)", DRM_FORMAT_ARGB8888 },
      { "XRGB8888 (BGRX bytes)", DRM_FORMAT_XRGB8888 },
      { "ABGR8888 (RGBA bytes)", DRM_FORMAT_ABGR8888 },
      { "XBGR8888 (RGBX bytes)", DRM_FORMAT_XBGR8888 },
   };
   for (unsigned i = 0; i < sizeof(fmts) / sizeof(fmts[0]); i++) {
      const int w = 64, h = 64;
      struct gbm_bo *bo = gbm_bo_create(gbm, w, h, fmts[i].fourcc, GBM_BO_USE_RENDERING | GBM_BO_USE_LINEAR);
      if (!bo) bo = gbm_bo_create(gbm, w, h, fmts[i].fourcc, GBM_BO_USE_RENDERING);
      if (!bo) { printf("%-24s gbm_bo_create failed\n", fmts[i].name); continue; }
      int dfd = gbm_bo_get_fd(bo);
      uint64_t mod = gbm_bo_get_modifier(bo);
      EGLint attr[] = {
         EGL_WIDTH, w, EGL_HEIGHT, h, EGL_LINUX_DRM_FOURCC_EXT, (EGLint)fmts[i].fourcc,
         EGL_DMA_BUF_PLANE0_FD_EXT, dfd, EGL_DMA_BUF_PLANE0_OFFSET_EXT, 0,
         EGL_DMA_BUF_PLANE0_PITCH_EXT, (EGLint)gbm_bo_get_stride(bo),
         EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT, (EGLint)(mod & 0xffffffff),
         EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT, (EGLint)(mod >> 32),
         EGL_NONE };
      if (mod == DRM_FORMAT_MOD_INVALID)
         attr[12] = EGL_NONE;
      EGLImageKHR img = create_image(dpy, EGL_NO_CONTEXT, EGL_LINUX_DMA_BUF_EXT, NULL, attr);
      if (img == EGL_NO_IMAGE_KHR) {
         printf("%-24s eglCreateImage failed 0x%x\n", fmts[i].name, eglGetError());
         continue;
      }
      GLuint tex, fbo;
      glGenTextures(1, &tex);
      glBindTexture(GL_TEXTURE_2D, tex);
      image_target(GL_TEXTURE_2D, img);
      glGenFramebuffers(1, &fbo);
      glBindFramebuffer(GL_FRAMEBUFFER, fbo);
      glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);
      GLenum st = glCheckFramebufferStatus(GL_FRAMEBUFFER);
      printf("%-24s mod 0x%llx  fbo status 0x%04x %s", fmts[i].name, (unsigned long long)mod, st,
             st == GL_FRAMEBUFFER_COMPLETE ? "COMPLETE" : st == 0x8CDD ? "UNSUPPORTED" : "?");
      if (draw && st == GL_FRAMEBUFFER_COMPLETE) {
         static const float q[] = { -1, -1, 1, -1, -1, 1, 1, 1 };
         glViewport(0, 0, w, h);
         glUseProgram(p);
         glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, q);
         glEnableVertexAttribArray(0);
         glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
         unsigned char px[4] = { 0 };
         glReadPixels(1, 1, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, px);
         glFinish();
         uint32_t stride; void *map_data = NULL;
         unsigned char *m = gbm_bo_map(bo, 0, 0, w, h, GBM_BO_TRANSFER_READ, &stride, &map_data);
         printf("  readpixels rgba=%d,%d,%d,%d", px[0], px[1], px[2], px[3]);
         if (m) {
            unsigned char *b = m + stride * (h / 2) + 4 * (w / 2);
            printf("  bo bytes=%d,%d,%d,%d (want rgba 204,102,51,255)", b[0], b[1], b[2], b[3]);
            gbm_bo_unmap(bo, map_data);
         }
      }
      printf("\n");
      glBindFramebuffer(GL_FRAMEBUFFER, 0);
      glDeleteFramebuffers(1, &fbo);
      glDeleteTextures(1, &tex);
      destroy_image(dpy, img);
      close(dfd);
      gbm_bo_destroy(bo);
   }
   return 0;
}
