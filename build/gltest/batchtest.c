/* batchtest: standalone repro for the grate draw-batching bug (Chromium needs GRATE_BATCH=0, notes/13).
 * Draws a deterministic pseudo-random sequence of Skia-like draws into a W x H FBO texture and writes
 * the read-back RGBA to a file. Run it twice on the Surface (GRATE_BATCH=0 and default batching) and
 * compare the files: they must be identical (same GPU, same commands).
 *
 *   batchtest OUT.rgba [NDRAWS [SEED]]
 * Feature switches (environment, all on by default, "0" turns one off) to bisect what breaks:
 *   BT_PROGS=mask   programs used: 1 solid, 2 rgba texture, 4 a8 coverage (glyph), 8 two textures,
 *                   16 per-vertex colour (default 31)
 *   BT_VBO=0        client-side vertex arrays instead of one streamed VBO (glBufferSubData)
 *   BT_ORPHAN=0     never re-specify the VBO with glBufferData(NULL) when it is full (wrap instead)
 *   BT_TEXSUB=0     no glTexSubImage2D into the textures between draws
 *   BT_CLEAR=0      no scissored glClear between draws
 *   BT_FBO=0        no switching to the second FBO (render-to-texture, sampled afterwards)
 *   BT_SCISSOR=0    no scissor test on draws
 *   BT_BLEND=0      no blending
 *   BT_INDEX=0      no indexed draws (default: half the draws use glDrawElements, element VBO or client indices)
 *   BT_UBYTE=0      per-vertex colours as floats instead of normalized GL_UNSIGNED_BYTE
 *   BT_LINEAR=0     NEAREST filtering only (default: some draws switch the textures to LINEAR)
 *   BT_MASK=0       no glColorMask changes
 *   BT_LARGE=1      every quad large (0.5..1 of the target): long fragment work per draw, which is what
 *                   exposes state of the next draw reaching the 3D unit before the previous draw's
 *                   fragments are done (GRATE_DRAW_WAIT=0 on mesa-grate >= 26.2.3-30)
 *   BT_BIG=0        no long shader with many uniforms and 4 texture units (program 32)
 * Build: build/gltest/build.sh
 */
#define GL_GLEXT_PROTOTYPES 1
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <GLES2/gl2ext.h>
#include <fcntl.h>
#include <gbm.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define W 512
#define H 384
#define TW 300 /* NPOT textures, like most of Chromium's */
#define TH 200
#define VBO_BYTES (64 * 1024)

static unsigned rng = 1;
static unsigned rnd(unsigned n) { rng = rng * 1103515245u + 12345u; return (rng >> 8) % n; }
static float frnd(void) { return rnd(10000) / 10000.0f; }

static int env_on(const char *name, int def)
{
   const char *v = getenv(name);
   return v ? atoi(v) : def;
}

static GLuint build(const char *vs, const char *fs)
{
   GLuint p = glCreateProgram();
   GLuint v = glCreateShader(GL_VERTEX_SHADER), f = glCreateShader(GL_FRAGMENT_SHADER);
   glShaderSource(v, 1, &vs, NULL); glCompileShader(v);
   glShaderSource(f, 1, &fs, NULL); glCompileShader(f);
   glAttachShader(p, v); glAttachShader(p, f);
   glBindAttribLocation(p, 0, "pos");
   glBindAttribLocation(p, 1, "uv");
   glBindAttribLocation(p, 2, "col");
   glLinkProgram(p);
   GLint ok = 0;
   glGetProgramiv(p, GL_LINK_STATUS, &ok);
   if (!ok) {
      char log[1024];
      glGetProgramInfoLog(p, sizeof log, NULL, log);
      fprintf(stderr, "link failed: %s\n", log);
      exit(2);
   }
   return p;
}

enum { P_SOLID, P_RGBA, P_A8, P_TWO, P_VCOL, P_BIG, NPROG };
static GLuint prog[NPROG];
static int prog_attribs[NPROG] = { 1, 2, 2, 2, 2, 2 }; /* pos, +uv or +col */

static void make_programs(void)
{
   const char *vs_pos = "attribute vec2 pos; void main() { gl_Position = vec4(pos, 0.0, 1.0); }";
   const char *vs_uv = "attribute vec2 pos; attribute vec2 uv; varying vec2 v_uv;\n"
                       "void main() { v_uv = uv; gl_Position = vec4(pos, 0.0, 1.0); }";
   const char *vs_two = "attribute vec2 pos; attribute vec2 uv; varying vec2 v_uv; varying vec2 v_uv2;\n"
                        "void main() { v_uv = uv; v_uv2 = uv.yx; gl_Position = vec4(pos, 0.0, 1.0); }";
   const char *vs_col = "attribute vec2 pos; attribute vec4 col; varying vec4 v_col;\n"
                        "void main() { v_col = col; gl_Position = vec4(pos, 0.0, 1.0); }";
   prog[P_SOLID] = build(vs_pos, "precision mediump float; uniform vec4 color;\n"
                                 "void main() { gl_FragColor = color; }");
   prog[P_RGBA] = build(vs_uv, "precision mediump float; uniform sampler2D t; uniform vec4 color; varying vec2 v_uv;\n"
                               "void main() { gl_FragColor = texture2D(t, v_uv) * color.a; }");
   prog[P_A8] = build(vs_uv, "precision mediump float; uniform sampler2D a8; uniform vec4 color; varying vec2 v_uv;\n"
                             "void main() { gl_FragColor = color * texture2D(a8, v_uv).a; }");
   prog[P_TWO] = build(vs_two, "precision mediump float; uniform sampler2D t; uniform sampler2D a8; uniform vec4 color;\n"
                               "varying vec2 v_uv; varying vec2 v_uv2;\n"
                               "void main() { gl_FragColor = texture2D(t, v_uv) * texture2D(a8, v_uv2).a * color.a; }");
   prog[P_BIG] = build(vs_uv, "precision mediump float; uniform sampler2D t; uniform sampler2D a8;\n"
                              "uniform sampler2D t2; uniform sampler2D t3; uniform vec4 color; uniform vec4 k[6];\n"
                              "varying vec2 v_uv;\n"
                              "void main() {\n"
                              "  vec4 a = texture2D(t, v_uv) * k[0] + k[1];\n"
                              "  vec4 b = texture2D(t2, v_uv.yx) * k[2];\n"
                              "  vec4 c = texture2D(t3, v_uv * 0.5) * k[3] + k[4];\n"
                              "  float m = texture2D(a8, v_uv).a;\n"
                              "  gl_FragColor = clamp((a * 0.4 + b * 0.3 + c * 0.3) * m * k[5].x, 0.0, 1.0) * color.a;\n"
                              "}");
   prog[P_VCOL] = build(vs_col, "precision mediump float; varying vec4 v_col; void main() { gl_FragColor = v_col; }");
}

int main(int argc, char **argv)
{
   if (argc < 2) {
      fprintf(stderr, "usage: batchtest OUT.rgba [NDRAWS [SEED]]\n");
      return 1;
   }
   int ndraws = argc > 2 ? atoi(argv[2]) : 2000;
   rng = argc > 3 ? (unsigned)atoi(argv[3]) : 1;
   int progs = env_on("BT_PROGS", 31), use_vbo = env_on("BT_VBO", 1), orphan = env_on("BT_ORPHAN", 1);
   int texsub = env_on("BT_TEXSUB", 1), clears = env_on("BT_CLEAR", 1), fbo_switch = env_on("BT_FBO", 1);
   int scissor = env_on("BT_SCISSOR", 1), blend = env_on("BT_BLEND", 1);
   int indexed = env_on("BT_INDEX", 1), ubyte = env_on("BT_UBYTE", 1), linear = env_on("BT_LINEAR", 1);
   int masks = env_on("BT_MASK", 1), large = env_on("BT_LARGE", 0);
   if (!env_on("BT_BIG", 1))
      progs &= ~32;
   else if (!getenv("BT_PROGS"))
      progs = 63;

   PFNEGLGETPLATFORMDISPLAYEXTPROC get_display = (void *)eglGetProcAddress("eglGetPlatformDisplayEXT");
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
   make_programs();

   /* textures: RGBA NPOT image, A8 NPOT "glyph atlas", both filled with patterns */
   unsigned char *img = malloc(TW * TH * 4), *a8 = malloc(TW * TH);
   for (int i = 0; i < TW * TH; i++) {
      img[4 * i] = rnd(256); img[4 * i + 1] = rnd(256); img[4 * i + 2] = rnd(256); img[4 * i + 3] = 255;
      a8[i] = (i / TW + i % TW) % 7 < 3 ? 255 : rnd(256);
   }
   GLuint tex[2];
   glGenTextures(2, tex);
   for (int i = 0; i < 2; i++) {
      glBindTexture(GL_TEXTURE_2D, tex[i]);
      glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
      glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
      glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
      glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
   }
   glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
   glBindTexture(GL_TEXTURE_2D, tex[0]);
   glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, TW, TH, 0, GL_RGBA, GL_UNSIGNED_BYTE, img);
   glBindTexture(GL_TEXTURE_2D, tex[1]);
   glTexImage2D(GL_TEXTURE_2D, 0, GL_ALPHA, TW, TH, 0, GL_ALPHA, GL_UNSIGNED_BYTE, a8);

   /* render targets: main W x H, second one TW x TH that is sampled later (as tex[0]'s stand-in) */
   GLuint rt[2], fbo[2];
   glGenTextures(2, rt);
   glGenFramebuffers(2, fbo);
   int rw[2] = { W, TW }, rh[2] = { H, TH };
   for (int i = 0; i < 2; i++) {
      glBindTexture(GL_TEXTURE_2D, rt[i]);
      glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
      glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
      glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
      glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
      glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, rw[i], rh[i], 0, GL_RGBA, GL_UNSIGNED_BYTE, NULL);
      glBindFramebuffer(GL_FRAMEBUFFER, fbo[i]);
      glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, rt[i], 0);
      if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) {
         fprintf(stderr, "fbo %d incomplete\n", i);
         return 1;
      }
      glClearColor(0.1f * i, 0.2f, 0.3f, 1.0f);
      glClear(GL_COLOR_BUFFER_BIT);
   }
   int cur = 0;
   glBindFramebuffer(GL_FRAMEBUFFER, fbo[0]);
   glViewport(0, 0, W, H);

   GLuint ebo;
   glGenBuffers(1, &ebo);
   glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, ebo);
   {
      unsigned short idx[4 * 6];
      for (int q = 0; q < 4; q++) {
         static const unsigned short quad_idx[6] = { 0, 1, 2, 1, 3, 2 };
         for (int k = 0; k < 6; k++)
            idx[q * 6 + k] = q * 4 + quad_idx[k];
      }
      glBufferData(GL_ELEMENT_ARRAY_BUFFER, sizeof idx, idx, GL_STATIC_DRAW);
   }
   glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, 0);
   GLuint vbo;
   glGenBuffers(1, &vbo);
   glBindBuffer(GL_ARRAY_BUFFER, vbo);
   glBufferData(GL_ARRAY_BUFFER, VBO_BYTES, NULL, GL_STREAM_DRAW);
   int vbo_off = 0;
   if (blend)
      glBlendFunc(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);

   int stats[8] = { 0 };
   for (int d = 0; d < ndraws; d++) {
      /* occasional non-draw operations, as in Chromium between draws */
      if (texsub && rnd(8) == 0) {
         int t = rnd(2), x = rnd(TW - 32), y = rnd(TH - 16), w = 8 + rnd(24), h = 8 + rnd(8);
         unsigned char buf[32 * 16 * 4];
         for (int i = 0; i < w * h * 4; i++)
            buf[i] = rnd(256);
         glBindTexture(GL_TEXTURE_2D, tex[t]);
         glTexSubImage2D(GL_TEXTURE_2D, 0, x, y, w, h, t ? GL_ALPHA : GL_RGBA, GL_UNSIGNED_BYTE, buf);
         stats[0]++;
      }
      if (clears && rnd(40) == 0) {
         glEnable(GL_SCISSOR_TEST);
         glScissor(rnd(rw[cur] / 2), rnd(rh[cur] / 2), 16 + rnd(64), 16 + rnd(64));
         glClearColor(frnd(), frnd(), frnd(), 1.0f);
         glClear(GL_COLOR_BUFFER_BIT);
         stats[1]++;
      }
      if (fbo_switch && rnd(60) == 0) {
         cur ^= 1;
         glBindFramebuffer(GL_FRAMEBUFFER, fbo[cur]);
         glViewport(0, 0, rw[cur], rh[cur]);
         stats[2]++;
      }

      int p;
      do p = rnd(NPROG); while (!(progs & (1 << p)));
      glUseProgram(prog[p]);
      /* the image texture is sometimes the second render target (render-to-texture), unless drawing into it */
      GLuint image = (fbo_switch && cur == 0 && rnd(3) == 0) ? rt[1] : tex[0];
      glActiveTexture(GL_TEXTURE0);
      glBindTexture(GL_TEXTURE_2D, p == P_A8 ? tex[1] : image);
      glActiveTexture(GL_TEXTURE1);
      glBindTexture(GL_TEXTURE_2D, tex[1]);
      glActiveTexture(GL_TEXTURE2);
      glBindTexture(GL_TEXTURE_2D, (fbo_switch && cur == 0) ? rt[1] : tex[0]);
      glActiveTexture(GL_TEXTURE3);
      glBindTexture(GL_TEXTURE_2D, tex[0]);
      glActiveTexture(GL_TEXTURE0);
      if (linear && rnd(10) == 0) {
         GLenum f = rnd(2) ? GL_LINEAR : GL_NEAREST;
         glBindTexture(GL_TEXTURE_2D, tex[rnd(2)]);
         glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, f);
         glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, f);
         glBindTexture(GL_TEXTURE_2D, p == P_A8 ? tex[1] : image);
      }
      if (masks) {
         if (rnd(12) == 0) glColorMask(rnd(2), rnd(2), rnd(2), 1);
         else glColorMask(1, 1, 1, 1);
      }
      GLint loc;
      if ((loc = glGetUniformLocation(prog[p], "t")) >= 0) glUniform1i(loc, 0);
      if ((loc = glGetUniformLocation(prog[p], "a8")) >= 0) glUniform1i(loc, p == P_TWO || p == P_BIG ? 1 : 0);
      if ((loc = glGetUniformLocation(prog[p], "t2")) >= 0) glUniform1i(loc, 2);
      if ((loc = glGetUniformLocation(prog[p], "t3")) >= 0) glUniform1i(loc, 3);
      if ((loc = glGetUniformLocation(prog[p], "k")) >= 0) {
         float k[24];
         for (int i = 0; i < 24; i++) k[i] = frnd();
         k[20] = 0.5f + frnd();
         glUniform4fv(loc, 6, k);
      }
      if ((loc = glGetUniformLocation(prog[p], "color")) >= 0) {
         float a = 0.3f + 0.7f * frnd();
         glUniform4f(loc, frnd() * a, frnd() * a, frnd() * a, a);
      }

      if (blend && rnd(4)) glEnable(GL_BLEND); else glDisable(GL_BLEND);
      if (scissor && rnd(3) == 0) {
         glEnable(GL_SCISSOR_TEST);
         glScissor(rnd(rw[cur] / 2), rnd(rh[cur] / 2), 32 + rnd(rw[cur] / 2), 32 + rnd(rh[cur] / 2));
      } else
         glDisable(GL_SCISSOR_TEST);

      /* 1..4 quads, small rects like glyphs or larger ones; indexed: 4 vertices per quad + indices */
      int use_idx = indexed && rnd(2), quads = 1 + rnd(4), per = use_idx ? 4 : 6, nv = quads * per;
      int stride = (ubyte ? 5 : 8) * sizeof(float);
      float v[4 * 6 * 8];
      for (int q = 0; q < quads; q++) {
         float sz = large ? 0.5f + 0.5f * frnd() : rnd(5) ? 0.02f + 0.08f * frnd() : 0.2f + 0.8f * frnd();
         float x0 = -1 + 2 * frnd() * (1 - sz), y0 = -1 + 2 * frnd() * (1 - sz), x1 = x0 + sz, y1 = y0 + sz;
         float u0 = frnd() * 0.8f, t0 = frnd() * 0.8f, u1 = u0 + 0.2f * frnd(), t1 = t0 + 0.2f * frnd();
         float c[4] = { frnd(), frnd(), frnd(), 1 };
         float tri[6][4] = { { x0, y0, u0, t0 }, { x1, y0, u1, t0 }, { x0, y1, u0, t1 },
                             { x1, y0, u1, t0 }, { x1, y1, u1, t1 }, { x0, y1, u0, t1 } };
         float quad4[4][4] = { { x0, y0, u0, t0 }, { x1, y0, u1, t0 }, { x0, y1, u0, t1 }, { x1, y1, u1, t1 } };
         for (int k = 0; k < per; k++) {
            float *o = (float *)((char *)v + (q * per + k) * stride);
            memcpy(o, use_idx ? quad4[k] : tri[k], 4 * sizeof(float));
            if (ubyte) {
               unsigned char *cb = (unsigned char *)(o + 4);
               for (int i = 0; i < 4; i++) cb[i] = (unsigned char)(c[i] * 255);
            } else
               memcpy(o + 4, c, 4 * sizeof(float));
         }
      }
      const char *base = (const char *)v;
      if (use_vbo) {
         int bytes = nv * stride;
         if (vbo_off + bytes > VBO_BYTES) {
            if (orphan)
               glBufferData(GL_ARRAY_BUFFER, VBO_BYTES, NULL, GL_STREAM_DRAW);
            vbo_off = 0;
            stats[3]++;
         }
         glBindBuffer(GL_ARRAY_BUFFER, vbo);
         glBufferSubData(GL_ARRAY_BUFFER, vbo_off, bytes, v);
         base = (const char *)(intptr_t)vbo_off;
         vbo_off += bytes;
      } else
         glBindBuffer(GL_ARRAY_BUFFER, 0);
      glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, stride, base);
      glEnableVertexAttribArray(0);
      glDisableVertexAttribArray(1);
      glDisableVertexAttribArray(2);
      if (prog_attribs[p] > 1) {
         if (p == P_VCOL) {
            if (ubyte)
               glVertexAttribPointer(2, 4, GL_UNSIGNED_BYTE, GL_TRUE, stride, base + 4 * sizeof(float));
            else
               glVertexAttribPointer(2, 4, GL_FLOAT, GL_FALSE, stride, base + 4 * sizeof(float));
            glEnableVertexAttribArray(2);
         } else {
            glVertexAttribPointer(1, 2, GL_FLOAT, GL_FALSE, stride, base + 2 * sizeof(float));
            glEnableVertexAttribArray(1);
         }
      }
      if (use_idx) {
         unsigned short idx[4 * 6];
         for (int q = 0; q < quads; q++) {
            static const unsigned short quad_idx[6] = { 0, 1, 2, 1, 3, 2 };
            for (int k = 0; k < 6; k++)
               idx[q * 6 + k] = q * 4 + quad_idx[k];
         }
         if (rnd(2)) {
            glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, ebo);
            glDrawElements(GL_TRIANGLES, quads * 6, GL_UNSIGNED_SHORT, 0);
            glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, 0);
         } else
            glDrawElements(GL_TRIANGLES, quads * 6, GL_UNSIGNED_SHORT, idx);
         stats[5]++;
         stats[4]++;
         continue;
      }
      glDrawArrays(GL_TRIANGLES, 0, nv);
      stats[4]++;
   }

   /* show the second target in a corner of the main one, then read the main one back */
   glBindFramebuffer(GL_FRAMEBUFFER, fbo[0]);
   glViewport(0, 0, W, H);
   glDisable(GL_SCISSOR_TEST);
   glDisable(GL_BLEND);
   glColorMask(1, 1, 1, 1);
   unsigned char *out = malloc(W * H * 4);
   glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, out);
   unsigned char *out2 = malloc(TW * TH * 4);
   glBindFramebuffer(GL_FRAMEBUFFER, fbo[1]);
   glReadPixels(0, 0, TW, TH, GL_RGBA, GL_UNSIGNED_BYTE, out2);
   FILE *f = fopen(argv[1], "wb");
   if (!f) { perror(argv[1]); return 1; }
   fwrite(out, 1, W * H * 4, f);
   fwrite(out2, 1, TW * TH * 4, f);
   fclose(f);
   printf("indexed %d ", stats[5]);
   printf("draws %d texsub %d clears %d fbo-switches %d vbo-wraps %d -> %s (%dx%d + %dx%d RGBA)\n", stats[4],
          stats[0], stats[1], stats[2], stats[3], argv[1], W, H, TW, TH);
   return 0;
}
