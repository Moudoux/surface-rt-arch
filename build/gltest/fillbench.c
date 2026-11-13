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
/* rtshell render kit (shell/shaders): colours as varyings (v_mul, v_add) vs as uniforms */
static const char *vs_ui =
   "attribute vec2 pos;\n"
   "uniform vec4 rect;\n"
   "varying vec2 uv;\n"
   "varying vec4 v_mul;\n"
   "varying vec4 v_add;\n"
   "void main() {\n"
   "  vec2 p = mix(rect.xy, rect.zw, pos);\n"
   "  uv = pos;\n"
   "  v_mul = vec4(0.9);\n"
   "  v_add = vec4(0.05, 0.0, 0.0, 0.05);\n"
   "  gl_Position = vec4(p, 0.0, 1.0);\n"
   "}\n";
static const char *vs_flip =
   "attribute vec2 pos;\n"
   "uniform vec4 rect;\n"
   "varying vec2 uv;\n"
   "void main() {\n"
   "  vec2 p = mix(rect.xy, rect.zw, pos);\n"
   "  uv = vec2(pos.x, 1.0 - pos.y);\n"
   "  gl_Position = vec4(p, 0.0, 1.0);\n"
   "}\n";
static const char *fs_ui_texplain =
   "precision mediump float;\n"
   "uniform sampler2D s;\n"
   "varying vec2 uv;\n"
   "varying vec4 v_mul;\n"
   "varying vec4 v_add;\n"
   "void main() { gl_FragColor = texture2D(s, uv) * v_mul + v_add; }\n";
static const char *fs_ui_texmul =
   "precision mediump float;\n"
   "uniform sampler2D s;\n"
   "varying vec2 uv;\n"
   "varying vec4 v_mul;\n"
   "void main() { gl_FragColor = texture2D(s, uv) * v_mul; }\n";
static const char *fs_ui_fill =
   "precision mediump float;\n"
   "varying vec4 v_add;\n"
   "void main() { gl_FragColor = v_add; }\n";
static const char *fs_ui_texuni =
   "precision mediump float;\n"
   "uniform sampler2D s;\n"
   "uniform vec4 um, ua;\n"
   "varying vec2 uv;\n"
   "void main() { gl_FragColor = texture2D(s, uv) * um + ua; }\n";
static int iters;
static GLint loc_rect;

static void quad(float x0, float y0, float x1, float y1)
{
   GLint prog = 0;
   glGetIntegerv(GL_CURRENT_PROGRAM, &prog);
   glUniform4f(glGetUniformLocation(prog, "rect"), x0, y0, x1, y1);
   (void)loc_rect;
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

static GLuint p_flip, p_icon, p_ui_texplain, p_ui_texmul, p_ui_fill, p_ui_texuni;
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
static void ui_draw(GLuint p, int blend)
{
   if (blend) { glEnable(GL_BLEND); glBlendFunc(GL_ONE, GL_ONE_MINUS_SRC_ALPHA); }
   glUseProgram(p); glBindTexture(GL_TEXTURE_2D, stex); quad(-1, -1, 1, 1);
   if (blend) glDisable(GL_BLEND);
}
static void t_flip(int i) { glUseProgram(p_flip); glBindTexture(GL_TEXTURE_2D, stex); quad(-1, -1, 1, 1); }
static void t_scissor(int i) { glEnable(GL_SCISSOR_TEST); glScissor(0, 0, W, H); t_tex(i); glDisable(GL_SCISSOR_TEST); }
static void t_texsmall(int i);
static void t_tex_finish(int i) { t_tex(i); glFinish(); }
static void t_texsmall_finish(int i) { t_texsmall(i); glFinish(); }
static void t_tiny_bigtex_finish(int i) { glUseProgram(p_tex); glBindTexture(GL_TEXTURE_2D, stex); quad(-1, -1, -1 + 2.0f / W, -1 + 2.0f / H); glFinish(); }
static void t_tiny_smalltex_finish(int i) { glUseProgram(p_tex); glBindTexture(GL_TEXTURE_2D, stex_small); quad(-1, -1, -1 + 2.0f / W, -1 + 2.0f / H); glFinish(); }
/* The render kit's exact setup: ui.vert, interleaved 36-byte vertices (pos, uv, q floats; mul, add u8
 * normalized), u16 index buffer, DrawElements. Variants: float colours, DrawArrays. */
static GLuint p_kit_all[3];
static GLuint p_kit, kit_vbo, kit_vbo_f, kit_ibo;
static const char *vs_kit =
   "uniform mat3 u_proj;\n"
   "attribute vec2 a_pos;\nattribute vec2 a_uv;\nattribute vec3 a_q;\nattribute vec4 a_mul;\nattribute vec4 a_add;\n"
   "varying vec2 v_uv;\nvarying vec3 v_q;\nvarying vec4 v_mul;\nvarying vec4 v_add;\n"
   "void main() { v_uv = a_uv; v_q = a_q; v_mul = a_mul; v_add = a_add;\n"
   "  gl_Position = vec4((u_proj * vec3(a_pos, 1.0)).xy, 0.0, 1.0); }\n";
static const char *vs_kit_noq =
   "uniform mat3 u_proj;\n"
   "attribute vec2 a_pos;\nattribute vec2 a_uv;\nattribute vec3 a_q;\nattribute vec4 a_mul;\nattribute vec4 a_add;\n"
   "varying vec2 v_uv;\nvarying vec4 v_mul;\nvarying vec4 v_add;\n"
   "void main() { v_uv = a_uv; v_mul = a_mul; v_add = a_add;\n"
   "  gl_Position = vec4((u_proj * vec3(a_pos, 1.0)).xy, 0.0, 1.0); }\n";
static const char *vs_kit_q_last =
   "uniform mat3 u_proj;\n"
   "attribute vec2 a_pos;\nattribute vec2 a_uv;\nattribute vec3 a_q;\nattribute vec4 a_mul;\nattribute vec4 a_add;\n"
   "varying vec2 v_uv;\nvarying vec4 v_mul;\nvarying vec4 v_add;\nvarying vec3 v_q;\n"
   "void main() { v_uv = a_uv; v_q = a_q; v_mul = a_mul; v_add = a_add;\n"
   "  gl_Position = vec4((u_proj * vec3(a_pos, 1.0)).xy, 0.0, 1.0); }\n";
static const char *fs_kit =
   "precision mediump float;\nuniform sampler2D u_tex;\n"
   "varying vec2 v_uv;\nvarying vec4 v_mul;\nvarying vec4 v_add;\n"
   "void main() { gl_FragColor = texture2D(u_tex, v_uv) * v_mul + v_add; }\n";
static void kit_attribs(int floats)
{
   int stride = floats ? 60 : 36;
   glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, stride, (void *)0);
   glVertexAttribPointer(1, 2, GL_FLOAT, GL_FALSE, stride, (void *)8);
   glVertexAttribPointer(2, 3, GL_FLOAT, GL_FALSE, stride, (void *)16);
   if (floats) {
      glVertexAttribPointer(3, 4, GL_FLOAT, GL_FALSE, stride, (void *)28);
      glVertexAttribPointer(4, 4, GL_FLOAT, GL_FALSE, stride, (void *)44);
   } else {
      glVertexAttribPointer(3, 4, GL_UNSIGNED_BYTE, GL_TRUE, stride, (void *)28);
      glVertexAttribPointer(4, 4, GL_UNSIGNED_BYTE, GL_TRUE, stride, (void *)32);
   }
   for (int k = 0; k < 5; k++) glEnableVertexAttribArray(k);
}
static void kit_draw(int floats, int indexed)
{
   glUseProgram(p_kit);
   glBindTexture(GL_TEXTURE_2D, stex);
   glBindBuffer(GL_ARRAY_BUFFER, floats ? kit_vbo_f : kit_vbo);
   kit_attribs(floats);
   if (indexed) {
      glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, kit_ibo);
      glDrawElements(GL_TRIANGLES, 6, GL_UNSIGNED_SHORT, 0);
      glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, 0);
   } else {
      glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
   }
   for (int k = 1; k < 5; k++) glDisableVertexAttribArray(k);
   /* restore fillbench's own layout (attribute 0 = pos from its VBO) */
   glBindBuffer(GL_ARRAY_BUFFER, 0);
}
static GLuint fb_vbo;
static void kit_restore(void)
{
   glBindBuffer(GL_ARRAY_BUFFER, fb_vbo);
   glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, 0);
   glEnableVertexAttribArray(0);
}
static void t_kit_exact(int i) { kit_draw(0, 1); kit_restore(); }
static void t_kit_floats(int i) { kit_draw(1, 1); kit_restore(); }
static void t_kit_arrays(int i) { kit_draw(0, 0); kit_restore(); }
/* Fragment variants on the kit's vertex shader (real per-vertex varyings, nothing folds). */
static const char *fs_kit_v[] = {
   "precision mediump float;uniform sampler2D u_tex;varying vec2 v_uv;"
   "void main(){gl_FragColor=texture2D(u_tex,v_uv);}",
   "precision mediump float;uniform sampler2D u_tex;varying vec2 v_uv;varying vec4 v_mul;"
   "void main(){gl_FragColor=texture2D(u_tex,v_uv)*v_mul;}",
   "precision mediump float;varying vec4 v_add;"
   "void main(){gl_FragColor=v_add;}",
   "precision mediump float;varying vec4 v_mul;varying vec4 v_add;"
   "void main(){gl_FragColor=v_mul*v_add;}",
   "precision mediump float;varying vec3 v_q;varying vec4 v_add;"
   "void main(){gl_FragColor=v_add*v_q.z;}",
   "precision mediump float;uniform sampler2D u_tex;varying vec2 v_uv;varying vec4 v_mul;varying vec4 v_add;"
   "void main(){gl_FragColor=texture2D(u_tex,v_uv)*v_mul+v_add;}",
};
static const char *fs_kit_names[] = { "rt kitvar tex(uv)", "rt kitvar tex*mul (uv,mul)", "rt kitvar fill (add)",
   "rt kitvar mul*add (2 vec4)", "rt kitvar add*q.z (vec4+vec3)", "rt kitvar tex*mul+add (3)" };
static GLuint p_kitvar[6];
static int kitvar_i;
static void t_kitvar(int i) { GLuint keep = p_kit; p_kit = p_kitvar[kitvar_i]; kit_draw(0, 1); kit_restore(); p_kit = keep; }
/* Proposed v2 kit: vertex = pos(2) + a(4) + b(4) floats; programs read one vec4 varying (two for textured
 * rounded edges); colours/tints are uniforms per batch. */
static const char *vs_v2 =
   "uniform mat3 u_proj;attribute vec2 a_pos;attribute vec4 a_a;attribute vec4 a_b;varying vec4 v_a;varying vec4 v_b;"
   "void main(){v_a=a_a;v_b=a_b;gl_Position=vec4((u_proj*vec3(a_pos,1.0)).xy,0.0,1.0);}";
static const char *fs_v2[] = {
   /* opaque texture */
   "precision mediump float;uniform sampler2D u_tex;varying vec4 v_a;"
   "void main(){gl_FragColor=texture2D(u_tex,v_a.xy);}",
   /* tex: (texel*mul+add)*opacity */
   "precision mediump float;uniform sampler2D u_tex;uniform vec4 u_mul,u_add;varying vec4 v_a;"
   "void main(){gl_FragColor=(texture2D(u_tex,v_a.xy)*u_mul+u_add)*v_a.z;}",
   /* tex: texel*mul*opacity (glyphs, icons, shadows) */
   "precision mediump float;uniform sampler2D u_tex;uniform vec4 u_mul;varying vec4 v_a;"
   "void main(){gl_FragColor=texture2D(u_tex,v_a.xy)*(u_mul*v_a.z);}",
   /* solid rounded edge: u_color * opacity * coverage(q) */
   "precision mediump float;uniform vec4 u_mul;varying vec4 v_a;"
   "void main(){vec2 q=v_a.xy;float d=length(max(q,0.0))+min(max(q.x,q.y),0.0)-v_a.z;"
   "gl_FragColor=u_mul*(v_a.w*clamp(0.5-d,0.0,1.0));}",
   /* textured rounded edge: two varyings */
   "precision mediump float;uniform sampler2D u_tex;uniform vec4 u_mul,u_add;varying vec4 v_a;varying vec4 v_b;"
   "void main(){vec2 q=v_b.xy;float d=length(max(q,0.0))+min(max(q.x,q.y),0.0)-v_b.z;"
   "gl_FragColor=(texture2D(u_tex,v_a.xy)*u_mul+u_add)*(v_a.z*clamp(0.5-d,0.0,1.0));}",
   /* fill: varying colour */
   "precision mediump float;varying vec4 v_a;void main(){gl_FragColor=v_a;}",
};
static const char *fs_v2_names[] = { "rt v2 opaque tex", "rt v2 (tex*mul+add)*a", "rt v2 tex*(mul*a)",
   "rt v2 solid edge (sdf)", "rt v2 tex edge (2 varyings)", "rt v2 fill" };
static GLuint p_v2[6], v2_vbo;
static int v2_i, v2_blend;
static void t_v2(int i)
{
   if (v2_blend) { glEnable(GL_BLEND); glBlendFunc(GL_ONE, GL_ONE_MINUS_SRC_ALPHA); }
   glUseProgram(p_v2[v2_i]);
   glBindTexture(GL_TEXTURE_2D, stex);
   glBindBuffer(GL_ARRAY_BUFFER, v2_vbo);
   glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 40, (void *)0);
   glVertexAttribPointer(1, 4, GL_FLOAT, GL_FALSE, 40, (void *)8);
   glVertexAttribPointer(2, 4, GL_FLOAT, GL_FALSE, 40, (void *)24);
   glEnableVertexAttribArray(1); glEnableVertexAttribArray(2);
   glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
   glDisableVertexAttribArray(1); glDisableVertexAttribArray(2);
   kit_restore();
   if (v2_blend) glDisable(GL_BLEND);
}
static void v2_setup(void)
{
   for (int k = 0; k < 6; k++) {
      GLuint pp = glCreateProgram();
      GLuint v = glCreateShader(GL_VERTEX_SHADER), f = glCreateShader(GL_FRAGMENT_SHADER);
      glShaderSource(v, 1, &vs_v2, NULL); glCompileShader(v);
      glShaderSource(f, 1, &fs_v2[k], NULL); glCompileShader(f);
      glAttachShader(pp, v); glAttachShader(pp, f);
      glBindAttribLocation(pp, 0, "a_pos"); glBindAttribLocation(pp, 1, "a_a"); glBindAttribLocation(pp, 2, "a_b");
      glLinkProgram(pp);
      GLint ok; glGetProgramiv(pp, GL_LINK_STATUS, &ok);
      if (!ok) { fprintf(stderr, "v2 %d link failed\n", k); exit(2); }
      glUseProgram(pp);
      glUniform1i(glGetUniformLocation(pp, "u_tex"), 0);
      float m[9] = { 2.0f / W, 0, 0,  0, -2.0f / H, 0,  -1, 1, 1 };
      glUniformMatrix3fv(glGetUniformLocation(pp, "u_proj"), 1, GL_FALSE, m);
      glUniform4f(glGetUniformLocation(pp, "u_mul"), 0.9, 0.8, 0.7, 0.9);
      glUniform4f(glGetUniformLocation(pp, "u_add"), 0.05, 0.0, 0.0, 0.05);
      p_v2[k] = pp;
   }
   /* TL TR BL BR; a = (u, v, opacity, w), b = (qx, qy, r, -): q spans an edge region so the sdf does work */
   float vb[4][10] = {
      { 0, 0,  0, 0, 0.9f, 0.5f,  -40, -30, 8, 0 },
      { W, 0,  1, 0, 0.9f, 0.6f,   8, -30, 8, 0 },
      { 0, H,  0, 1, 0.8f, 0.7f,  -40, 8, 8, 0 },
      { W, H,  1, 1, 0.8f, 0.8f,   8, 8, 8, 0 },
   };
   glGenBuffers(1, &v2_vbo); glBindBuffer(GL_ARRAY_BUFFER, v2_vbo);
   glBufferData(GL_ARRAY_BUFFER, sizeof(vb), vb, GL_STATIC_DRAW);
   glBindBuffer(GL_ARRAY_BUFFER, 0);
}
static void t_kit_noq(int i) { GLuint keep = p_kit; p_kit = p_kit_all[1]; kit_draw(0, 1); kit_restore(); p_kit = keep; }
static void t_kit_qlast(int i) { GLuint keep = p_kit; p_kit = p_kit_all[2]; kit_draw(0, 1); kit_restore(); p_kit = keep; }
static void kit_setup(void)
{
  const char *vss[3] = { vs_kit, vs_kit_noq, vs_kit_q_last };
  for (int pi = 0; pi < 3; pi++) {
   p_kit = glCreateProgram();
   GLuint v = glCreateShader(GL_VERTEX_SHADER), f = glCreateShader(GL_FRAGMENT_SHADER);
   glShaderSource(v, 1, &vss[pi], NULL); glCompileShader(v);
   glShaderSource(f, 1, &fs_kit, NULL); glCompileShader(f);
   glAttachShader(p_kit, v); glAttachShader(p_kit, f);
   const char *names[5] = { "a_pos", "a_uv", "a_q", "a_mul", "a_add" };
   for (int k = 0; k < 5; k++) glBindAttribLocation(p_kit, k, names[k]);
   glLinkProgram(p_kit);
   GLint ok; glGetProgramiv(p_kit, GL_LINK_STATUS, &ok);
   if (!ok) { fprintf(stderr, "kit link failed\n"); exit(2); }
   glUseProgram(p_kit);
   glUniform1i(glGetUniformLocation(p_kit, "u_tex"), 0);
   /* pixels -> clip, y down (as Smithay's projection) */
   float m[9] = { 2.0f / W, 0, 0,  0, -2.0f / H, 0,  -1, 1, 1 };
   glUniformMatrix3fv(glGetUniformLocation(p_kit, "u_proj"), 1, GL_FALSE, m);
   p_kit_all[pi] = p_kit;
  }
  for (int vi = 0; vi < 6; vi++) {
   GLuint pp = glCreateProgram();
   GLuint v = glCreateShader(GL_VERTEX_SHADER), f = glCreateShader(GL_FRAGMENT_SHADER);
   glShaderSource(v, 1, &vs_kit, NULL); glCompileShader(v);
   glShaderSource(f, 1, &fs_kit_v[vi], NULL); glCompileShader(f);
   glAttachShader(pp, v); glAttachShader(pp, f);
   const char *nm[5] = { "a_pos", "a_uv", "a_q", "a_mul", "a_add" };
   for (int k = 0; k < 5; k++) glBindAttribLocation(pp, k, nm[k]);
   glLinkProgram(pp);
   GLint ok; glGetProgramiv(pp, GL_LINK_STATUS, &ok);
   if (!ok) { fprintf(stderr, "kitvar %d link failed\n", vi); exit(2); }
   glUseProgram(pp);
   glUniform1i(glGetUniformLocation(pp, "u_tex"), 0);
   float m[9] = { 2.0f / W, 0, 0,  0, -2.0f / H, 0,  -1, 1, 1 };
   glUniformMatrix3fv(glGetUniformLocation(pp, "u_proj"), 1, GL_FALSE, m);
   p_kitvar[vi] = pp;
  }
  p_kit = p_kit_all[0];
   /* corner order TL TR BL BR; strip order for DrawArrays is the same */
   float pos[4][2] = { {0, 0}, {W, 0}, {0, H}, {W, H} }, uv[4][2] = { {0, 0}, {1, 0}, {0, 1}, {1, 1} };
   unsigned char vb[4 * 36], *b = vb;
   float vf[4 * 15], *f15 = vf;
   for (int k = 0; k < 4; k++, b += 36, f15 += 15) {
      float fl[7] = { pos[k][0], pos[k][1], uv[k][0], uv[k][1], -4096, -4096, 0 };
      memcpy(b, fl, 28);
      unsigned char col[8] = { 230, 230, 230, 230, 12, 0, 0, 12 };
      memcpy(b + 28, col, 8);
      memcpy(f15, fl, 28);
      for (int c = 0; c < 8; c++) f15[7 + c] = col[c] / 255.0f;
   }
   glGenBuffers(1, &kit_vbo); glBindBuffer(GL_ARRAY_BUFFER, kit_vbo);
   glBufferData(GL_ARRAY_BUFFER, sizeof(vb), vb, GL_STREAM_DRAW);
   glGenBuffers(1, &kit_vbo_f); glBindBuffer(GL_ARRAY_BUFFER, kit_vbo_f);
   glBufferData(GL_ARRAY_BUFFER, sizeof(vf), vf, GL_STREAM_DRAW);
   unsigned short idx[6] = { 0, 1, 2, 2, 1, 3 };
   glGenBuffers(1, &kit_ibo); glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, kit_ibo);
   glBufferData(GL_ELEMENT_ARRAY_BUFFER, sizeof(idx), idx, GL_STATIC_DRAW);
   glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, 0);
}
static void t_ui_texplain(int i) { ui_draw(p_ui_texplain, 0); }
static void t_ui_texplain_b(int i) { ui_draw(p_ui_texplain, 1); }
static void t_ui_texmul(int i) { ui_draw(p_ui_texmul, 0); }
static void t_ui_texuni(int i) { ui_draw(p_ui_texuni, 0); }
static void t_ui_texuni_b(int i) { ui_draw(p_ui_texuni, 1); }
static void t_ui_fill_b(int i) { ui_draw(p_ui_fill, 1); }
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
   p_ui_texplain = program(vs_ui, fs_ui_texplain);
   p_flip = program(vs_flip, fs_tex);
   glUseProgram(p_flip);
   glUniform1i(glGetUniformLocation(p_flip, "s"), 0);
   p_ui_texmul = program(vs_ui, fs_ui_texmul);
   p_ui_fill = program(vs_ui, fs_ui_fill);
   p_ui_texuni = program(vs_ui, fs_ui_texuni);
   GLuint progs[8] = { p_tex, p_col, p_texmod, p_icon, p_ui_texplain, p_ui_texmul, p_ui_fill, p_ui_texuni };
   for (int k = 0; k < 8; k++) {
      glUseProgram(progs[k]);
      glUniform1i(glGetUniformLocation(progs[k], "s"), 0);
   }
   glUseProgram(p_col);
   glUniform4f(glGetUniformLocation(p_col, "c"), 0.2, 0.4, 0.6, 1.0);
   glUseProgram(p_texmod);
   glUniform4f(glGetUniformLocation(p_texmod, "modulation"), 0.9, 0.9, 0.9, 0.9);
   glUseProgram(p_ui_texuni);
   glUniform4f(glGetUniformLocation(p_ui_texuni, "um"), 0.9, 0.9, 0.9, 0.9);
   glUniform4f(glGetUniformLocation(p_ui_texuni, "ua"), 0.05, 0.0, 0.0, 0.05);
   glUseProgram(p_icon);
   glUniform1f(glGetUniformLocation(p_icon, "opacity"), 1.0);
   glUniform1f(glGetUniformLocation(p_icon, "highlight_amount"), 0.0);
   glUniform1f(glGetUniformLocation(p_icon, "desaturate_amount"), 0.0);
   loc_rect = glGetUniformLocation(p_tex, "rect");
   if (loc_rect != glGetUniformLocation(p_col, "rect") || loc_rect != glGetUniformLocation(p_texmod, "rect") ||
       loc_rect != glGetUniformLocation(p_icon, "rect"))
      fprintf(stderr, "warning: rect uniform locations differ\n");

   static const float verts[] = { 0, 0, 1, 0, 0, 1, 1, 1 };
   kit_setup();
   v2_setup();
   GLuint vbo;
   glGenBuffers(1, &vbo);
   fb_vbo = vbo;
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
   B("rt kit exact (u8 colours, indexed)", t_kit_exact, iters, fs);
   for (v2_blend = 0; v2_blend < 2; v2_blend++)
      for (v2_i = 0; v2_i < 6; v2_i++) {
         char nm[64];
         snprintf(nm, sizeof nm, "%s%s", fs_v2_names[v2_i], v2_blend ? " blended" : "");
         B(nm, t_v2, iters, fs);
      }
   for (kitvar_i = 0; kitvar_i < 6; kitvar_i++) B(fs_kit_names[kitvar_i], t_kitvar, iters, fs);
   B("rt kit without v_q", t_kit_noq, iters, fs);
   B("rt kit with v_q declared last", t_kit_qlast, iters, fs);
   B("rt kit float colours, indexed", t_kit_floats, iters, fs);
   B("rt kit u8 colours, DrawArrays", t_kit_arrays, iters, fs);
   B("rt tex 1366x768 + glFinish", t_tex_finish, iters, fs);
   B("rt tex 64x64 + glFinish", t_texsmall_finish, iters, fs);
   B("rt tiny draw, 1366x768 tex + glFinish", t_tiny_bigtex_finish, iters, 0);
   B("rt tiny draw, 64x64 tex + glFinish", t_tiny_smalltex_finish, iters, 0);
   B("rt tex, v flipped (as Smithay)", t_flip, iters, fs);
   B("rt tex, scissor test on", t_scissor, iters, fs);
   B("rt texplain (2 colour varyings)", t_ui_texplain, iters, fs);
   B("rt texplain blended", t_ui_texplain_b, iters, fs);
   B("rt tex*v_mul (1 colour varying)", t_ui_texmul, iters, fs);
   B("rt tex*um+ua (uniforms)", t_ui_texuni, iters, fs);
   B("rt tex*um+ua blended", t_ui_texuni_b, iters, fs);
   B("rt fill (varying) blended", t_ui_fill_b, iters, fs);
   B("100 draws 64x64 (per iter)", t_small_draws, iters, 100 * 64.0 * 64);
   B("100 draws 64x64 + glFinish", t_small_draws_finish, iters, 100 * 64.0 * 64);
   B("1 tiny draw + glFinish", t_tiny_finish, iters * 3, 0);
   B("texsubimage 1366x768", t_upload, iters, fs);
   B("texsubimage 256x64", t_upload_small, iters * 3, 256 * 64);
   B("texsubimage 1366x768 + draw it", t_upload_draw, iters, fs);
   B("readpixels 1366x16", t_readpix, iters, W * 16.0);
   return 0;
}
