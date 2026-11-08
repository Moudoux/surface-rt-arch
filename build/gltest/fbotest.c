/* FBO completeness test for grate: RGBA8 colour texture + various depth/stencil
 * renderbuffers, desktop GL (as Qt uses it) and GLES2. Prints the status of
 * glCheckFramebufferStatus for each combination, then clears + draws nothing,
 * reads one pixel back after a clear to check the FBO works.
 *   fbotest [gl|gles]
 * Build: build/gltest/build.sh
 */
#define GL_GLEXT_PROTOTYPES 1
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GL/gl.h>
#include <GL/glext.h>
#include <stdio.h>
#include <string.h>

struct combo { const char *name; GLenum ds; GLenum attach; GLenum depth2; GLenum stencil2; };

int main(int argc, char **argv)
{
   int gles = argc > 1 && !strcmp(argv[1], "gles");
   PFNEGLGETPLATFORMDISPLAYEXTPROC get_display =
      (void *)eglGetProcAddress("eglGetPlatformDisplayEXT");
   EGLDisplay dpy = get_display(EGL_PLATFORM_SURFACELESS_MESA, EGL_DEFAULT_DISPLAY, NULL);
   if (!eglInitialize(dpy, NULL, NULL)) { fprintf(stderr, "eglInitialize failed\n"); return 1; }
   eglBindAPI(gles ? EGL_OPENGL_ES_API : EGL_OPENGL_API);
   static const EGLint es_attr[] = { EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE };
   EGLContext ctx = eglCreateContext(dpy, EGL_NO_CONFIG_KHR, EGL_NO_CONTEXT, gles ? es_attr : NULL);
   if (ctx == EGL_NO_CONTEXT || !eglMakeCurrent(dpy, EGL_NO_SURFACE, EGL_NO_SURFACE, ctx)) {
      fprintf(stderr, "context failed: 0x%x\n", eglGetError());
      return 1;
   }
   printf("%s: %s / %s\n", gles ? "GLES" : "GL", glGetString(GL_RENDERER), glGetString(GL_VERSION));
   const char *ext = (const char *)glGetString(GL_EXTENSIONS);
   printf("packed_depth_stencil ext: %s\n",
          ext && (strstr(ext, "GL_EXT_packed_depth_stencil") || strstr(ext, "GL_OES_packed_depth_stencil")) ? "yes" : "no/core");

   static const struct combo c[] = {
      {"none", 0, 0, 0, 0},
      {"D24S8 depth_stencil", GL_DEPTH24_STENCIL8, GL_DEPTH_STENCIL_ATTACHMENT, 0, 0},
      {"D24S8 depth+stencil pts", GL_DEPTH24_STENCIL8, 1, 0, 0},
      {"D16", GL_DEPTH_COMPONENT16, GL_DEPTH_ATTACHMENT, 0, 0},
      {"D24", GL_DEPTH_COMPONENT24, GL_DEPTH_ATTACHMENT, 0, 0},
      {"S8", GL_STENCIL_INDEX8, GL_STENCIL_ATTACHMENT, 0, 0},
      {"D16 + S8 separate", 0, 0, GL_DEPTH_COMPONENT16, GL_STENCIL_INDEX8},
   };
   int fails = 0;
   for (unsigned i = 0; i < sizeof(c) / sizeof(c[0]); i++) {
      while (glGetError()) ;
      GLuint tex, fb, rb[2] = {0, 0};
      glGenTextures(1, &tex);
      glBindTexture(GL_TEXTURE_2D, tex);
      glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, 64, 64, 0, GL_RGBA, GL_UNSIGNED_BYTE, NULL);
      glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
      glGenFramebuffers(1, &fb);
      glBindFramebuffer(GL_FRAMEBUFFER, fb);
      glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);
      glGenRenderbuffers(2, rb);
      if (c[i].ds) {
         glBindRenderbuffer(GL_RENDERBUFFER, rb[0]);
         glRenderbufferStorage(GL_RENDERBUFFER, c[i].ds, 64, 64);
         if (c[i].attach == 1) {
            glFramebufferRenderbuffer(GL_FRAMEBUFFER, GL_DEPTH_ATTACHMENT, GL_RENDERBUFFER, rb[0]);
            glFramebufferRenderbuffer(GL_FRAMEBUFFER, GL_STENCIL_ATTACHMENT, GL_RENDERBUFFER, rb[0]);
         } else {
            glFramebufferRenderbuffer(GL_FRAMEBUFFER, c[i].attach, GL_RENDERBUFFER, rb[0]);
         }
      }
      if (c[i].depth2) {
         glBindRenderbuffer(GL_RENDERBUFFER, rb[0]);
         glRenderbufferStorage(GL_RENDERBUFFER, c[i].depth2, 64, 64);
         glFramebufferRenderbuffer(GL_FRAMEBUFFER, GL_DEPTH_ATTACHMENT, GL_RENDERBUFFER, rb[0]);
         glBindRenderbuffer(GL_RENDERBUFFER, rb[1]);
         glRenderbufferStorage(GL_RENDERBUFFER, c[i].stencil2, 64, 64);
         glFramebufferRenderbuffer(GL_FRAMEBUFFER, GL_STENCIL_ATTACHMENT, GL_RENDERBUFFER, rb[1]);
      }
      GLenum err = glGetError();
      GLenum st = glCheckFramebufferStatus(GL_FRAMEBUFFER);
      unsigned char px[4] = {0};
      if (st == GL_FRAMEBUFFER_COMPLETE) {
         glViewport(0, 0, 64, 64);
         glClearColor(0.25f, 0.5f, 0.75f, 1.0f);
         glClearDepthf(1.0f);
         glClearStencil(0);
         glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT | GL_STENCIL_BUFFER_BIT);
         glReadPixels(5, 5, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, px);
      }
      GLint dbits = -1, sbits = -1;
      if (rb[0] && (c[i].ds || c[i].depth2)) {
         glBindRenderbuffer(GL_RENDERBUFFER, rb[0]);
         glGetRenderbufferParameteriv(GL_RENDERBUFFER, GL_RENDERBUFFER_DEPTH_SIZE, &dbits);
         glGetRenderbufferParameteriv(GL_RENDERBUFFER, GL_RENDERBUFFER_STENCIL_SIZE, &sbits);
      }
      printf("%-26s status 0x%04x %s  glerr 0x%x  rb d%d s%d  clear px %d %d %d %d (err after 0x%x)\n",
             c[i].name, st, st == GL_FRAMEBUFFER_COMPLETE ? "COMPLETE  " : "INCOMPLETE",
             err, dbits, sbits, px[0], px[1], px[2], px[3], glGetError());
      if (st != GL_FRAMEBUFFER_COMPLETE) fails++;
      glBindFramebuffer(GL_FRAMEBUFFER, 0);
      glDeleteFramebuffers(1, &fb);
      glDeleteRenderbuffers(2, rb);
      glDeleteTextures(1, &tex);
   }
   return fails ? 3 : 0;
}
