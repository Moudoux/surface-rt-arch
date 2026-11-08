/*
 * fillbench: rough GPU/driver cost measurements for grate (surfaceless EGL,
 * GLES2, rendering into a 1366x768 BGRA texture FBO like a compositor).
 * Each test runs N iterations then glFinish(); prints ms per iteration,
 * Mpx/s and the CPU share (process CPU time / wall time).
 *   ./fillbench [iterations]
 */
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <GLES2/gl2ext.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#define W 1366
#define H 768

static double now(clockid_t c)
{
   struct timespec t;
   clock_gettime(c, &t);
   return t.tv_sec + t.tv_nsec * 1e-9;
}

static GLuint shader(GLenum type, const char *src)
{
   GLuint s = glCreateShader(type);
   glShaderSource(s, 1, &src, NULL);
   glCompileShader(s);
   GLint ok;
   glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
   if (!ok) { fprintf(stderr, "compile failed\n"); exit(2); }
   return s;
}

static GLuint program(const char *vs, const char *fs)
{
   GLuint p = glCreateProgram();
   glAttachShader(p, shader(GL_VERTEX_SHADER, vs));
   glAttachShader(p, shader(GL_FRAGMENT_SHADER, fs));
   glBindAttribLocation(p, 0, "pos");
   glLinkProgram(p);
   GLint ok;
   glGetProgramiv(p, GL_LINK_STATUS, &ok);
   if (!ok) { fprintf(stderr, "link failed\n"); exit(2); }
   return p;
}

static const char *vs_src =
   "attribute vec2 pos;\n"
   "uniform vec4 rect;\n" /* x0 y0 x1 y1 in NDC */
   "varying vec2 uv;\n"
   "void main() {\n"
   "  vec2 p = mix(rect.xy, rect.zw, pos);\n"
   "  uv = pos;\n"
   "  gl_Position = vec4(p, 0.0, 1.0);\n"
   "}\n";
static const char *fs_tex =
   "precision mediump float;\n"
   "uniform sampler2D s;\n"
   "varying vec2 uv;\n"
   "void main() { gl_FragColor = texture2D(s, uv); }\n";
static const char *fs_col =
   "precision mediump float;\n"
   "uniform vec4 c;\n"
   "void main() { gl_FragColor = c; }\n";
/* KWin-like: texture * opacity/modulation */
static const char *fs_texmod =
   "precision mediump float;\n"
   "uniform sampler2D s;\n"
   "uniform vec4 modulation;\n"
   "varying vec2 uv;\n"
   "void main() { gl_FragColor = texture2D(s, uv) * modulation; }\n";

/* Kirigami.Icon (kirigami 6.31 icon.frag, icon_default): highlight gamma,
 * desaturate mix, opacity; uniforms as for an icon at rest (0, 0, 1) */
static const char *fs_icon =
   "precision mediump float;\n"
   "uniform sampler2D s;\n"
   "uniform float opacity, highlight_amount, desaturate_amount;\n"
   "varying vec2 uv;\n"
   "void main() {\n"
   "  vec4 color = texture2D(s, uv);\n"
   "  float gamma = 1.0 / (2.0 * max(0.25, highlight_amount * color.a) + 0.5);\n"
   "  color.rgb = pow(color.rgb, vec3(gamma));\n"
   "  color = mix(color, vec4(vec3(dot(color.rgb, vec3(0.2126, 0.7152, 0.0722))) * 0.5, color.a * 0.5), desaturate_amount);\n"
   "  gl_FragColor = color * opacity;\n"
   "}\n";
static int iters;
static GLint loc_rect;

static void quad(float x0, float y0, float x1, float y1)
{
   glUniform4f(loc_rect, x0, y0, x1, y1);
   glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
}

typedef void (*fn)(int i);

static void bench(const char *name, fn f, int n, double px_per_iter)
{
   f(0); glFinish(); /* warm-up: shader compile, allocation */
   double w0 = now(CLOCK_MONOTONIC), c0 = now(CLOCK_PROCESS_CPUTIME_ID);
   for (int i = 0; i < n; i++)
      f(i);
   glFinish();
   double w = now(CLOCK_MONOTONIC) - w0, c = now(CLOCK_PROCESS_CPUTIME_ID) - c0;
   printf("%-34s %8.2f ms/iter %8.1f Mpx/s  cpu %3.0f%%\n", name, 1e3 * w / n,
          px_per_iter > 0 ? px_per_iter * n / w / 1e6 : 0.0, 100 * c / w);
}

static GLuint p_icon;
static GLuint p_tex, p_col, p_texmod, stex, stex_small;
static unsigned char *upload;

static void t_clear(int i) { glClearColor(i & 1, 0.5, 0.2, 1); glClear(GL_COLOR_BUFFER_BIT); }
static void t_clear_finish(int i) { t_clear(i); glFinish(); }
static void t_solid(int i) { glUseProgram(p_col); quad(-1, -1, 1, 1); }
static void t_tex(int i) { glUseProgram(p_tex); glBindTexture(GL_TEXTURE_2D, stex); quad(-1, -1, 1, 1); }
static void t_texmod(int i) { glUseProgram(p_texmod); glBindTexture(GL_TEXTURE_2D, stex); quad(-1, -1, 1, 1); }
static void t_tex_blend(int i)
{
   glEnable(GL_BLEND); glBlendFunc(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
   t_texmod(i);
   glDisable(GL_BLEND);
}
/* KWin's usual window draw: plain texture, premultiplied source-over */
static void t_tex_plain_blend(int i)
{
   glEnable(GL_BLEND); glBlendFunc(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
   t_tex(i);
   glDisable(GL_BLEND);
}
static void t_icon_blend(int i)
{
   glEnable(GL_BLEND); glBlendFunc(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
   glUseProgram(p_icon); glBindTexture(GL_TEXTURE_2D, stex); quad(-1, -1, 1, 1);
   glDisable(GL_BLEND);
}
static void t_texsmall(int i) { glUseProgram(p_tex); glBindTexture(GL_TEXTURE_2D, stex_small); quad(-1, -1, 1, 1); }
/* 100 small 32x32 quads per iteration: per-draw overhead */
static void t_small_draws(int i)
{
   glUseProgram(p_tex); glBindTexture(GL_TEXTURE_2D, stex);
   for (int k = 0; k < 100; k++) {
      float x = -1 + (k % 20) * 0.1f, y = -1 + (k / 20) * 0.1f;
      quad(x, y, x + 64.0f / W, y + 64.0f / H);
   }
}
static void t_small_draws_finish(int i) { t_small_draws(i); glFinish(); }
/* 100 small draws, each with a uniform change and a texture change */
static void t_tiny_finish(int i) { glUseProgram(p_tex); quad(-1, -1, -1 + 2.0f / W, -1 + 2.0f / H); glFinish(); }
static void t_upload(int i)
{
   glBindTexture(GL_TEXTURE_2D, stex);
   glTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, W, H, GL_BGRA_EXT, GL_UNSIGNED_BYTE, upload);
}
static void t_upload_small(int i)
{
   glBindTexture(GL_TEXTURE_2D, stex);
   glTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, 256, 64, GL_BGRA_EXT, GL_UNSIGNED_BYTE, upload);
}
static void t_upload_draw(int i) { t_upload(i); t_tex(i); }
static void t_readpix(int i) { glReadPixels(0, 0, W, 16, GL_RGBA, GL_UNSIGNED_BYTE, upload); }

int main(int argc, char **argv)
{
   iters = argc > 1 ? atoi(argv[1]) : 30;
   PFNEGLGETPLATFORMDISPLAYEXTPROC get_display =
      (void *)eglGetProcAddress("eglGetPlatformDisplayEXT");
   EGLDisplay dpy = get_display(EGL_PLATFORM_SURFACELESS_MESA, EGL_DEFAULT_DISPLAY, NULL);
   if (!eglInitialize(dpy, NULL, NULL)) { fprintf(stderr, "eglInitialize failed\n"); return 1; }
   eglBindAPI(EGL_OPENGL_ES_API);
   static const EGLint cfg_attr[] = { EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT, EGL_NONE };
   EGLConfig cfg = NULL;
   EGLint n = 0;
   eglChooseConfig(dpy, cfg_attr, &cfg, 1, &n);
   static const EGLint ctx_attr[] = { EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE };
   EGLContext ctx = eglCreateContext(dpy, n ? cfg : EGL_NO_CONFIG_KHR, EGL_NO_CONTEXT, ctx_attr);
   if (ctx == EGL_NO_CONTEXT || !eglMakeCurrent(dpy, EGL_NO_SURFACE, EGL_NO_SURFACE, ctx)) {
      fprintf(stderr, "context failed: 0x%x\n", eglGetError());
      return 1;
   }
   printf("renderer: %s, %dx%d target, %d iterations\n", glGetString(GL_RENDERER), W, H, iters);

   GLuint rt, fbo;
   glGenTextures(1, &rt);
   glBindTexture(GL_TEXTURE_2D, rt);
   glTexImage2D(GL_TEXTURE_2D, 0, GL_BGRA_EXT, W, H, 0, GL_BGRA_EXT, GL_UNSIGNED_BYTE, NULL);
   glGenFramebuffers(1, &fbo);
   glBindFramebuffer(GL_FRAMEBUFFER, fbo);
   glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, rt, 0);
   if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) { fprintf(stderr, "fbo\n"); return 1; }
   glViewport(0, 0, W, H);

   upload = malloc(W * H * 4);
   for (int k = 0; k < W * H * 4; k++) upload[k] = k * 7;
   glActiveTexture(GL_TEXTURE0);
   GLuint t[2];
   glGenTextures(2, t);
   stex = t[0]; stex_small = t[1];
   glBindTexture(GL_TEXTURE_2D, stex);
   glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
   glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
   glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
   glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
   glTexImage2D(GL_TEXTURE_2D, 0, GL_BGRA_EXT, W, H, 0, GL_BGRA_EXT, GL_UNSIGNED_BYTE, upload);
   glBindTexture(GL_TEXTURE_2D, stex_small);
   glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
   glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
   glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
   glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
   glTexImage2D(GL_TEXTURE_2D, 0, GL_BGRA_EXT, 64, 64, 0, GL_BGRA_EXT, GL_UNSIGNED_BYTE, upload);

   p_tex = program(vs_src, fs_tex);
   p_col = program(vs_src, fs_col);
   p_texmod = program(vs_src, fs_texmod);
   p_icon = program(vs_src, fs_icon);
   GLuint progs[4] = { p_tex, p_col, p_texmod, p_icon };
   for (int k = 0; k < 4; k++) {
      glUseProgram(progs[k]);
      glUniform1i(glGetUniformLocation(progs[k], "s"), 0);
   }
   glUseProgram(p_col);
   glUniform4f(glGetUniformLocation(p_col, "c"), 0.2, 0.4, 0.6, 1.0);
   glUseProgram(p_texmod);
   glUniform4f(glGetUniformLocation(p_texmod, "modulation"), 0.9, 0.9, 0.9, 0.9);
   glUseProgram(p_icon);
   glUniform1f(glGetUniformLocation(p_icon, "opacity"), 1.0);
   glUniform1f(glGetUniformLocation(p_icon, "highlight_amount"), 0.0);
   glUniform1f(glGetUniformLocation(p_icon, "desaturate_amount"), 0.0);
   loc_rect = glGetUniformLocation(p_tex, "rect");
   if (loc_rect != glGetUniformLocation(p_col, "rect") || loc_rect != glGetUniformLocation(p_texmod, "rect") ||
       loc_rect != glGetUniformLocation(p_icon, "rect"))
      fprintf(stderr, "warning: rect uniform locations differ\n");

   static const float verts[] = { 0, 0, 1, 0, 0, 1, 1, 1 };
   GLuint vbo;
   glGenBuffers(1, &vbo);
   glBindBuffer(GL_ARRAY_BUFFER, vbo);
   glBufferData(GL_ARRAY_BUFFER, sizeof(verts), verts, GL_STATIC_DRAW);
   glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, 0);
   glEnableVertexAttribArray(0);

   const double fs = (double)W * H;
   const char *only = getenv("FB_ONLY");
#define B(name, f, n, px) if (!only || strstr(name, only)) bench(name, f, n, px)
   B("clear (gr2d fill)", t_clear, iters, fs);
   B("clear + glFinish", t_clear_finish, iters, fs);
   B("solid colour quad", t_solid, iters, fs);
   B("textured quad 1366x768 src", t_tex, iters, fs);
   B("textured quad 64x64 src", t_texsmall, iters, fs);
   B("tex*modulation quad", t_texmod, iters, fs);
   B("tex*modulation blended quad", t_tex_blend, iters, fs);
   B("textured blended quad", t_tex_plain_blend, iters, fs);
   B("kirigami icon shader blended", t_icon_blend, iters, fs);
   B("100 draws 64x64 (per iter)", t_small_draws, iters, 100 * 64.0 * 64);
   B("100 draws 64x64 + glFinish", t_small_draws_finish, iters, 100 * 64.0 * 64);
   B("1 tiny draw + glFinish", t_tiny_finish, iters * 3, 0);
   B("texsubimage 1366x768", t_upload, iters, fs);
   B("texsubimage 256x64", t_upload_small, iters * 3, 256 * 64);
   B("texsubimage 1366x768 + draw it", t_upload_draw, iters, fs);
   B("readpixels 1366x16", t_readpix, iters, W * 16.0);
   return 0;
}
