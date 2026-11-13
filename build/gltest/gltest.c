/* Headless GLES2 shader test for grate (Surface RT).
 * Renders a fullscreen quad into a 64x64 BGRA FBO with a given fragment
 * shader, then prints a few pixels. No display or session needed (EGL
 * surfaceless platform on the render node).
 *
 *   gltest <frag.glsl> [x y ...]     pixels default to 4 points
 *
 * Vertex shader: a_pos (clip xy) -> v_uv in [0,1], v_col = vec4(v_uv, 0.25, 1.0).
 * Uniforms available to the fragment shader (if declared):
 *   uniform vec4 u_a = (0.5, 0.25, 0.75, 1.0), u_b = (2.0, 0.5, 4.0, 0.125);
 *   uniform sampler2D u_tex: 4x4 texture, texel (i,j) = (i/3, j/3, 0.5, 1).
 * Other uniforms: "//! uniform NAME values..." lines in the shader; "//! filter linear"
 * samples u_tex (or any sampler: unit 0) bilinearly.
 * GLTEST_BLEND=1: clear to (0.2, 0.4, 0.6, 0.8), draw with premultiplied
 * source-over blending (GL_ONE, GL_ONE_MINUS_SRC_ALPHA).
 * GLTEST_BLEND="src dst [srcA dstA [eq [eqA]]]": same background, these
 * factors (one zero sc isc sa isa dc idc da ida cc icc ca ica sat) and
 * equations (add sub rsub min max).
 * GLTEST_BLEND_COLOR="r g b a" (default 0.3 0.6 0.9 0.5) for glBlendColor.
 * GLTEST_RGBA=1: RGBA render target (default BGRA).
 * GLTEST_VS=file: own vertex shader (attribute a_pos, same varyings/uniforms).
 * GLTEST_SCISSOR="x y w h": scissor the draw (GL window coordinates).
 * GLTEST_COLORMASK="rgba" as 0/1 digits, e.g. 1101 (works without blending).
 * Exit status: 0 drawn, 2 compile/link failure, 1 setup failure.
 * Build: build/gltest/build.sh (armv7h, in the srt-mesa-build image).
 */
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <GLES2/gl2ext.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define W 64
#define H 64

static GLenum blend_factor(const char *name)
{
   static const struct { const char *n; GLenum f; } t[] = {
      {"one", GL_ONE}, {"zero", GL_ZERO}, {"sc", GL_SRC_COLOR},
      {"isc", GL_ONE_MINUS_SRC_COLOR}, {"sa", GL_SRC_ALPHA},
      {"isa", GL_ONE_MINUS_SRC_ALPHA}, {"dc", GL_DST_COLOR},
      {"idc", GL_ONE_MINUS_DST_COLOR}, {"da", GL_DST_ALPHA},
      {"ida", GL_ONE_MINUS_DST_ALPHA}, {"cc", GL_CONSTANT_COLOR},
      {"icc", GL_ONE_MINUS_CONSTANT_COLOR}, {"ca", GL_CONSTANT_ALPHA},
      {"ica", GL_ONE_MINUS_CONSTANT_ALPHA}, {"sat", GL_SRC_ALPHA_SATURATE},
      {"add", GL_FUNC_ADD}, {"sub", GL_FUNC_SUBTRACT},
      {"rsub", GL_FUNC_REVERSE_SUBTRACT}, {"min", 0x8007}, {"max", 0x8008},
   };
   for (unsigned i = 0; i < sizeof(t) / sizeof(t[0]); i++)
      if (!strcmp(name, t[i].n))
         return t[i].f;
   fprintf(stderr, "unknown blend factor/equation %s\n", name);
   exit(1);
}

static char *slurp(const char *path)
{
   FILE *f = fopen(path, "rb");
   if (!f) { perror(path); exit(1); }
   fseek(f, 0, SEEK_END);
   long n = ftell(f);
   rewind(f);
   char *s = malloc(n + 1);
   if (fread(s, 1, n, f) != (size_t)n) { perror(path); exit(1); }
   s[n] = 0;
   fclose(f);
   return s;
}

static GLuint compile(GLenum type, const char *src)
{
   GLuint s = glCreateShader(type);
   glShaderSource(s, 1, &src, NULL);
   glCompileShader(s);
   GLint ok;
   glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
   if (!ok) {
      char log[4096];
      glGetShaderInfoLog(s, sizeof(log), NULL, log);
      printf("COMPILE FAILED (%s):\n%s\n", type == GL_VERTEX_SHADER ? "vs" : "fs", log);
      exit(2);
   }
   return s;
}

int main(int argc, char **argv)
{
   if (argc < 2) {
      fprintf(stderr, "usage: %s frag.glsl [x y ...]\n", argv[0]);
      return 1;
   }
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
   printf("renderer: %s\n", glGetString(GL_RENDERER));

   GLuint tex;
   glGenTextures(1, &tex);
   glBindTexture(GL_TEXTURE_2D, tex);
   if (getenv("GLTEST_RGBA")) /* RGBA render target instead of BGRA */
      glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, W, H, 0, GL_RGBA, GL_UNSIGNED_BYTE, NULL);
   else
      glTexImage2D(GL_TEXTURE_2D, 0, GL_BGRA_EXT, W, H, 0, GL_BGRA_EXT, GL_UNSIGNED_BYTE, NULL);
   GLuint fbo;
   glGenFramebuffers(1, &fbo);
   glBindFramebuffer(GL_FRAMEBUFFER, fbo);
   glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);
   GLenum st = glCheckFramebufferStatus(GL_FRAMEBUFFER);
   if (st != GL_FRAMEBUFFER_COMPLETE) { fprintf(stderr, "fbo status 0x%x\n", st); return 1; }

   /* sampled texture */
   unsigned char texels[4 * 4 * 4];
   for (int j = 0; j < 4; j++)
      for (int i = 0; i < 4; i++) {
         unsigned char *p = texels + (j * 4 + i) * 4; /* BGRA */
         p[2] = i * 85; p[1] = j * 85; p[0] = 128; p[3] = 255 - (i + j) * 30;
      }
   GLuint stex;
   glActiveTexture(GL_TEXTURE0);
   glGenTextures(1, &stex);
   glBindTexture(GL_TEXTURE_2D, stex);
   glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
   glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
   glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
   glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
   glTexImage2D(GL_TEXTURE_2D, 0, GL_BGRA_EXT, 4, 4, 0, GL_BGRA_EXT, GL_UNSIGNED_BYTE, texels);

   static const char *vs_src =
      "attribute vec2 a_pos;\n"
      "varying vec2 v_uv;\n"
      "varying vec4 v_col;\n"
      "void main() {\n"
      "  v_uv = a_pos * 0.5 + 0.5;\n"
      "  v_col = vec4(v_uv, 0.25, 1.0);\n"
      "  gl_Position = vec4(a_pos, 0.0, 1.0);\n"
      "}\n";
   GLuint prog = glCreateProgram();
   const char *vs_file = getenv("GLTEST_VS");
   glAttachShader(prog, compile(GL_VERTEX_SHADER, vs_file ? slurp(vs_file) : vs_src));
   glAttachShader(prog, compile(GL_FRAGMENT_SHADER, slurp(argv[1])));
   glBindAttribLocation(prog, 0, "a_pos");
   glLinkProgram(prog);
   GLint ok;
   glGetProgramiv(prog, GL_LINK_STATUS, &ok);
   if (!ok) {
      char log[4096];
      glGetProgramInfoLog(prog, sizeof(log), NULL, log);
      printf("LINK FAILED:\n%s\n", log);
      return 2;
   }
   glUseProgram(prog);
   GLint loc;
   if ((loc = glGetUniformLocation(prog, "u_a")) >= 0) glUniform4f(loc, 0.5, 0.25, 0.75, 1.0);
   if ((loc = glGetUniformLocation(prog, "u_b")) >= 0) glUniform4f(loc, 2.0, 0.5, 4.0, 0.125);
   if ((loc = glGetUniformLocation(prog, "u_tex")) >= 0) glUniform1i(loc, 0);
   /* Directives in the fragment shader source:
    *   //! uniform NAME v0 v1 ...   float/vec/mat3 uniform or array (missing values 0)
    *   //! filter linear            bilinear filtering of the sampled texture */
   {
      const char *src = slurp(argv[1]);
      if (strstr(src, "//! filter linear")) {
         glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
         glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
      }
      for (const char *p = src; (p = strstr(p, "//! uniform ")); ) {
         char name[128];
         int used = 0;
         p += 12;
         if (sscanf(p, "%127s%n", name, &used) != 1) break;
         p += used;
         static float v[256];
         int n = 0;
         memset(v, 0, sizeof(v));
         while (n < 256) {
            char *end;
            while (*p == ' ') p++;
            if (*p == '\n' || !*p) break;
            float f = strtof(p, &end);
            if (end == p) break;
            v[n++] = f;
            p = end;
         }
         GLint count = 0;
         glGetProgramiv(prog, GL_ACTIVE_UNIFORMS, &count);
         for (GLint u = 0; u < count; u++) {
            char un[160];
            GLint size;
            GLenum type;
            glGetActiveUniform(prog, u, sizeof(un), NULL, &size, &type, un);
            char *br = strchr(un, '[');
            if (br) *br = 0;
            if (strcmp(un, name)) continue;
            loc = glGetUniformLocation(prog, name);
            switch (type) {
            case GL_FLOAT:      glUniform1fv(loc, size, v); break;
            case GL_FLOAT_VEC2: glUniform2fv(loc, size, v); break;
            case GL_FLOAT_VEC3: glUniform3fv(loc, size, v); break;
            case GL_FLOAT_VEC4: glUniform4fv(loc, size, v); break;
            case GL_FLOAT_MAT3: glUniformMatrix3fv(loc, size, GL_FALSE, v); break;
            default: fprintf(stderr, "directive: unsupported type of %s\n", name); break;
            }
         }
      }
   }

   static const float quad[] = { -1, -1, 1, -1, -1, 1, 1, 1 };
   glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, quad);
   glEnableVertexAttribArray(0);
   glViewport(0, 0, W, H);
   const char *blend = getenv("GLTEST_BLEND");
   const char *mask = getenv("GLTEST_COLORMASK");
   if (blend || mask) {
      /* blend onto a known background (default: premultiplied source-over) */
      glClearColor(0.2, 0.4, 0.6, 0.8);
      glClear(GL_COLOR_BUFFER_BIT);
   }
   if (mask && strlen(mask) == 4)
      glColorMask(mask[0] == '1', mask[1] == '1', mask[2] == '1', mask[3] == '1');
   if (blend) {
      char w[6][16] = {"one", "isa", "", "", "add", ""};
      int n = strcmp(blend, "1") ? sscanf(blend, "%15s %15s %15s %15s %15s %15s",
                                          w[0], w[1], w[2], w[3], w[4], w[5]) : 2;
      if (n < 4) { strcpy(w[2], w[0]); strcpy(w[3], w[1]); }
      if (n < 6) strcpy(w[5], w[4]);
      float c[4] = {0.3, 0.6, 0.9, 0.5};
      const char *bc = getenv("GLTEST_BLEND_COLOR");
      if (bc) sscanf(bc, "%f %f %f %f", &c[0], &c[1], &c[2], &c[3]);
      glBlendColor(c[0], c[1], c[2], c[3]);
      glEnable(GL_BLEND);
      glBlendFuncSeparate(blend_factor(w[0]), blend_factor(w[1]),
                          blend_factor(w[2]), blend_factor(w[3]));
      glBlendEquationSeparate(blend_factor(w[4]), blend_factor(w[5]));
      if (glGetError()) { fprintf(stderr, "blend state rejected by GL\n"); return 2; }
   } else if (mask) {
      glDisable(GL_BLEND);
   } else {
      glDisable(GL_BLEND);
      glClearColor(1.0, 0.0, 1.0, 1.0); /* magenta = nothing drawn */
      glClear(GL_COLOR_BUFFER_BIT);
   }
   const char *sc = getenv("GLTEST_SCISSOR");
   int sr[4];
   if (sc && sscanf(sc, "%d %d %d %d", &sr[0], &sr[1], &sr[2], &sr[3]) == 4) {
      glEnable(GL_SCISSOR_TEST);
      glScissor(sr[0], sr[1], sr[2], sr[3]);
   }
   glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
   glFinish();

   static unsigned char px[W * H * 4];
   glReadPixels(0, 0, W, H, GL_BGRA_EXT, GL_UNSIGNED_BYTE, px);
   GLenum err = glGetError();
   if (err) printf("GL error 0x%x\n", err);

   int defaults[] = { 8, 8, 56, 8, 8, 56, 32, 32 };
   int *pts = defaults, npts = 4;
   int custom[64];
   if (argc > 3) {
      npts = 0;
      for (int i = 2; i + 1 < argc && npts < 32; i += 2) {
         custom[npts * 2] = atoi(argv[i]);
         custom[npts * 2 + 1] = atoi(argv[i + 1]);
         npts++;
      }
      pts = custom;
   }
   for (int i = 0; i < npts; i++) {
      int x = pts[i * 2], y = pts[i * 2 + 1];
      unsigned char *p = px + (y * W + x) * 4;
      /* uv at pixel centre, for comparing against the expected formula */
      printf("px %2d,%2d uv=(%.3f,%.3f) rgba=(%.3f %.3f %.3f %.3f)\n", x, y,
             (x + 0.5) / W, (y + 0.5) / H, p[2] / 255.0, p[1] / 255.0, p[0] / 255.0, p[3] / 255.0);
   }
   return 0;
}
