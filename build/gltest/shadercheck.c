/* Compile-and-draw check for captured desktop GLSL shaders (Qt uses a GL 2.0
 * context with "#version 120"). For each file a partner shader is generated
 * (a VS writing every varying the FS reads, or an FS summing every varying
 * the VS writes), all active uniforms get generic values (samplers unit 0,
 * floats 0.5, ints 0, matrices identity), one quad is drawn into a 64x64 FBO.
 * grate's rejections show up on stderr ("shader rejected", "UNIMPLEMENTED").
 *
 *   shadercheck FS_xxx.glsl|VS_xxx.glsl|x.frag|x.vert ...
 * Prints per file: "<name> drew rgba=(...)" or "<name> COMPILE/LINK FAILED".
 * GLES2 shaders (KWin's: "#extension GL_OES_..." without #version) get a GLES 2
 * context instead (also "#version 100", ANGLE/Chromium), detected from the first file, or forced with SHADERCHECK_ES=1.
 * Build: build/gltest/build.sh (also builds this).
 */
#define GL_GLEXT_PROTOTYPES 1
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GL/gl.h>
#include <GL/glext.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static char *slurp(const char *path)
{
   FILE *f = fopen(path, "rb");
   if (!f) { perror(path); exit(1); }
   fseek(f, 0, SEEK_END);
   long n = ftell(f);
   fseek(f, 0, SEEK_SET);
   char *s = malloc(n + 1);
   if (fread(s, 1, n, f) != (size_t)n) { perror(path); exit(1); }
   s[n] = 0;
   fclose(f);
   return s;
}

static GLuint compile(GLenum type, const char *src, const char *what)
{
   GLuint s = glCreateShader(type);
   glShaderSource(s, 1, &src, NULL);
   glCompileShader(s);
   GLint ok;
   glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
   if (!ok) {
      char log[4096];
      glGetShaderInfoLog(s, sizeof(log), NULL, log);
      printf("%s COMPILE FAILED (%s):\n%s\n", what, type == GL_VERTEX_SHADER ? "vs" : "fs", log);
      return 0;
   }
   return s;
}

/* Collect "varying <type> <name>;" declarations (one per line). */
static int varyings(const char *src, char types[][16], char names[][64], int max)
{
   int n = 0;
   for (const char *p = src; (p = strstr(p, "varying ")) && n < max; p++) {
      if (p != src && p[-1] != '\n')
         continue;
      if (sscanf(p, "varying %15s %63[A-Za-z0-9_]", types[n], names[n]) == 2) {
         if (!strcmp(types[n], "highp") || !strcmp(types[n], "mediump") || !strcmp(types[n], "lowp"))
            sscanf(p, "varying %*s %15s %63[A-Za-z0-9_]", types[n], names[n]);
         bool dup = false; /* same name in several #if branches */
         for (int i = 0; i < n; i++)
            dup |= !strcmp(names[i], names[n]);
         if (!dup)
            n++;
      }
   }
   return n;
}

static const char *swz(const char *type)
{
   return !strcmp(type, "float") ? ".x" : !strcmp(type, "vec2") ? ".xy" :
          !strcmp(type, "vec3") ? ".xyz" : "";
}

int main(int argc, char **argv)
{
   PFNEGLGETPLATFORMDISPLAYEXTPROC get_display =
      (void *)eglGetProcAddress("eglGetPlatformDisplayEXT");
   EGLDisplay dpy = get_display(EGL_PLATFORM_SURFACELESS_MESA, EGL_DEFAULT_DISPLAY, NULL);
   if (!eglInitialize(dpy, NULL, NULL)) { fprintf(stderr, "eglInitialize failed\n"); return 1; }
   bool es = getenv("SHADERCHECK_ES") != NULL;
   if (argc > 1) {
      char *first = slurp(argv[1]);
      es |= (strstr(first, "#extension GL_OES") && !strstr(first, "#version 1")) ||
            strstr(first, "#version 100") != NULL; /* ESSL 1.00 (Chromium/ANGLE) */
      free(first);
   }
   static const EGLint es_attribs[] = {EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE};
   eglBindAPI(es ? EGL_OPENGL_ES_API : EGL_OPENGL_API);
   EGLContext ctx = eglCreateContext(dpy, EGL_NO_CONFIG_KHR, EGL_NO_CONTEXT, es ? es_attribs : NULL);
   if (ctx == EGL_NO_CONTEXT || !eglMakeCurrent(dpy, EGL_NO_SURFACE, EGL_NO_SURFACE, ctx)) {
      fprintf(stderr, "context failed: 0x%x\n", eglGetError());
      return 1;
   }
   fprintf(stderr, "renderer: %s, GL %s\n", glGetString(GL_RENDERER), glGetString(GL_VERSION));

   GLuint tex, fbo, stex;
   glGenTextures(1, &tex);
   glBindTexture(GL_TEXTURE_2D, tex);
   glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, 64, 64, 0, es ? GL_RGBA : GL_BGRA, GL_UNSIGNED_BYTE, NULL);
   glGenFramebuffers(1, &fbo);
   glBindFramebuffer(GL_FRAMEBUFFER, fbo);
   glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);
   if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) {
      fprintf(stderr, "fbo incomplete\n");
      return 1;
   }
   unsigned char texels[4 * 4 * 4];
   for (int i = 0; i < 64; i++)
      texels[i] = (i * 37) & 0xff;
   glGenTextures(1, &stex);
   glBindTexture(GL_TEXTURE_2D, stex);
   glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
   glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
   glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, 4, 4, 0, es ? GL_RGBA : GL_BGRA, GL_UNSIGNED_BYTE, texels);
   glViewport(0, 0, 64, 64);

   for (int a = 1; a < argc; a++) {
      const char *path = argv[a], *base = strrchr(path, '/');
      base = base ? base + 1 : path;
      bool is_vs = !strncmp(base, "VS_", 3) || strstr(base, ".vert");
      char *src = slurp(path);
      char types[32][16], names[32][64];
      int nv = varyings(src, types, names, 32);
      char partner[8192];
      int len = snprintf(partner, sizeof(partner),
                         es ? "#version 100\nprecision highp float;\n" : "#version 120\n");
      if (is_vs) {
         for (int i = 0; i < nv; i++)
            len += snprintf(partner + len, sizeof(partner) - len, "varying %s %s;\n", types[i], names[i]);
         len += snprintf(partner + len, sizeof(partner) - len, "void main() {\n  vec4 c = vec4(0.0);\n");
         for (int i = 0; i < nv; i++)
            len += snprintf(partner + len, sizeof(partner) - len, "  c%s += %s;\n", swz(types[i]), names[i]);
         snprintf(partner + len, sizeof(partner) - len, "  gl_FragColor = c;\n}\n");
      } else {
         len += snprintf(partner + len, sizeof(partner) - len, "attribute vec4 a_pos;\n");
         for (int i = 0; i < nv; i++)
            len += snprintf(partner + len, sizeof(partner) - len, "varying %s %s;\n", types[i], names[i]);
         len += snprintf(partner + len, sizeof(partner) - len,
                         "void main() {\n  vec4 v = a_pos * 0.5 + 0.5; v.w = 1.0;\n");
         for (int i = 0; i < nv; i++)
            len += snprintf(partner + len, sizeof(partner) - len, "  %s = %s(v%s);\n", names[i],
                            types[i], swz(types[i]));
         snprintf(partner + len, sizeof(partner) - len,
                  "  gl_Position = vec4(a_pos.xy, 0.0, 1.0);\n}\n");
      }
      fprintf(stderr, "=== %s\n", base);
      GLuint vs = compile(GL_VERTEX_SHADER, is_vs ? src : partner, base);
      GLuint fs = compile(GL_FRAGMENT_SHADER, is_vs ? partner : src, base);
      if (!vs || !fs) continue;
      GLuint prog = glCreateProgram();
      glAttachShader(prog, vs);
      glAttachShader(prog, fs);
      glLinkProgram(prog);
      GLint ok;
      glGetProgramiv(prog, GL_LINK_STATUS, &ok);
      if (!ok) {
         char log[4096];
         glGetProgramInfoLog(prog, sizeof(log), NULL, log);
         printf("%s LINK FAILED:\n%s\n", base, log);
         continue;
      }
      glUseProgram(prog);
      GLint nu;
      glGetProgramiv(prog, GL_ACTIVE_UNIFORMS, &nu);
      for (GLint i = 0; i < nu; i++) {
         char name[256];
         GLint size;
         GLenum type;
         glGetActiveUniform(prog, i, sizeof(name), NULL, &size, &type, name);
         GLint loc = glGetUniformLocation(prog, name);
         static const float id[16] = {1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1};
         static const float id3[9] = {1, 0, 0, 0, 1, 0, 0, 0, 1};
         switch (type) {
         case GL_FLOAT: glUniform1f(loc, 0.5); break;
         case GL_FLOAT_VEC2: glUniform2f(loc, 0.5, 0.5); break;
         case GL_FLOAT_VEC3: glUniform3f(loc, 0.5, 0.5, 0.5); break;
         case GL_FLOAT_VEC4: glUniform4f(loc, 0.5, 0.5, 0.5, 0.5); break;
         case GL_FLOAT_MAT4: glUniformMatrix4fv(loc, 1, GL_FALSE, id); break;
         case GL_FLOAT_MAT3: glUniformMatrix3fv(loc, 1, GL_FALSE, id3); break;
         case GL_INT: case GL_BOOL: case GL_SAMPLER_2D: glUniform1i(loc, 0); break;
         default: break;
         }
      }
      /* every active attribute gets the quad (x, y, 0, 1) */
      GLint na;
      glGetProgramiv(prog, GL_ACTIVE_ATTRIBUTES, &na);
      static const float quad[] = {-1, -1, 0, 1, 1, -1, 0, 1, -1, 1, 0, 1, 1, 1, 0, 1};
      for (GLint i = 0; i < 16; i++)
         glDisableVertexAttribArray(i);
      for (GLint i = 0; i < na; i++) {
         char name[256];
         GLint size;
         GLenum type;
         glGetActiveAttrib(prog, i, sizeof(name), NULL, &size, &type, name);
         GLint loc = glGetAttribLocation(prog, name);
         if (loc < 0) continue;
         glVertexAttribPointer(loc, 4, GL_FLOAT, GL_FALSE, 0, quad);
         glEnableVertexAttribArray(loc);
      }
      glClearColor(1, 0, 1, 1);
      glClear(GL_COLOR_BUFFER_BIT);
      glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
      unsigned char px[4];
      glReadPixels(32, 32, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, px);
      printf("%s drew rgba=(%.2f %.2f %.2f %.2f), %d uniforms, %d attributes\n", base,
             px[0] / 255.0, px[1] / 255.0, px[2] / 255.0, px[3] / 255.0, nu, na);
      fflush(stdout);
      glDeleteProgram(prog);
   }
   return 0;
}
