// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT
//
// reftrace: who takes the references that keep a GObject alive.
//
// Preloaded into one process, this logs the creation (g_object_new and its
// valist/with_properties forms, called from outside libgobject) and every
// g_object_ref, g_object_ref_sink and g_object_unref of objects whose type name
// is REFTRACE_TYPE, with a raw backtrace, to REFTRACE_OUT.<pid>, and copies
// /proc/self/maps next to it on the first event. Without both variables, or when
// the log cannot be created, it only forwards the calls.
// The environment is left alone, so a wrapper in between (a shell, a session
// script) passes it on; every process logs to its own REFTRACE_OUT.<pid>.
// reftrace.py turns the log into the sites that take references and never give
// them back.
//
//   make build/reftrace.so
//   LD_PRELOAD=$PWD/build/reftrace.so REFTRACE_TYPE=AdwApplicationWindow
//       REFTRACE_OUT=/some/dir/trace program
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdarg.h>
#include <execinfo.h>
#include <glib-object.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static FILE *out;
static const char *wanted;
static int maps_saved;
static __thread int inside;
static gpointer (*real_ref) (gpointer);
static gpointer (*real_ref_sink) (gpointer);
static void (*real_unref) (gpointer);
static GObject *(*real_new_valist) (GType, const gchar *, va_list);
static GObject *(*real_new_with_properties) (GType, guint, const char **, const GValue *);

// Runs before main, so before any other thread: as a constructor, or earlier from
// the first hook when another library's constructor makes a GObject first.
__attribute__ ((constructor)) static void
open_log (void)
{
  static int ready;
  if (ready)
    return;
  ready = 1;
  real_ref = dlsym (RTLD_NEXT, "g_object_ref");
  real_ref_sink = dlsym (RTLD_NEXT, "g_object_ref_sink");
  real_unref = dlsym (RTLD_NEXT, "g_object_unref");
  real_new_valist = dlsym (RTLD_NEXT, "g_object_new_valist");
  real_new_with_properties = dlsym (RTLD_NEXT, "g_object_new_with_properties");
  const char *prefix = getenv ("REFTRACE_OUT");
  wanted = getenv ("REFTRACE_TYPE");
  if (!prefix || !*prefix || !wanted || !*wanted)
    return;
  char path[4096];
  if (snprintf (path, sizeof path, "%s.%d", prefix, getpid ()) < (int) sizeof path)
    out = fopen (path, "we");
}

static void
save_maps (void)
{
  char path[4096], line[1024];
  if (snprintf (path, sizeof path, "%s.%d.maps", getenv ("REFTRACE_OUT"), getpid ()) >= (int) sizeof path)
    return;
  FILE *maps = fopen ("/proc/self/maps", "r"), *copy = fopen (path, "w");
  while (maps && copy && fgets (line, sizeof line, maps))
    fputs (line, copy);
  if (maps)
    fclose (maps);
  if (copy)
    fclose (copy);
}

static void
record (char kind, gpointer object)
{
  if (inside || !object || !wanted || !out)
    return;
  inside = 1;
  GTypeInstance *instance = object;
  if (instance->g_class && strcmp (g_type_name (instance->g_class->g_type), wanted) == 0)
    {
      if (!maps_saved)
        {
          maps_saved = 1;
          save_maps ();
        }
      void *frames[24];
      int count = backtrace (frames, 24);
      // One line per event even when several threads trace at once.
      flockfile (out);
      // The count before the operation; a creation has nothing before it.
      fprintf (out, "%c %p %u", kind, object,
               kind == 'N' ? 0 : (guint) g_atomic_int_get (&((GObject *) object)->ref_count));
      for (int i = 2; i < count; i++)
        fprintf (out, " %p", frames[i]);
      fputc ('\n', out);
      fflush (out);
      funlockfile (out);
    }
  inside = 0;
}

gpointer
(g_object_ref) (gpointer object)
{
  open_log ();
  record ('R', object);
  return real_ref (object);
}

// 's': sinking a floating reference takes it over and adds none.
gpointer
(g_object_ref_sink) (gpointer object)
{
  open_log ();
  record (object && g_object_is_floating (object) ? 's' : 'S', object);
  return real_ref_sink (object);
}

void
g_object_unref (gpointer object)
{
  open_log ();
  record ('U', object);
  real_unref (object);
}

// Creations. g_object_new's own call into g_object_new_valist stays inside
// libgobject and is not seen twice.
gpointer
g_object_new (GType type, const gchar *first_property, ...)
{
  open_log ();
  va_list args;
  va_start (args, first_property);
  GObject *object = real_new_valist (type, first_property, args);
  va_end (args);
  record ('N', object);
  return object;
}

GObject *
g_object_new_valist (GType type, const gchar *first_property, va_list args)
{
  open_log ();
  GObject *object = real_new_valist (type, first_property, args);
  record ('N', object);
  return object;
}

GObject *
g_object_new_with_properties (GType type, guint n, const char *names[], const GValue values[])
{
  open_log ();
  GObject *object = real_new_with_properties (type, n, names, values);
  record ('N', object);
  return object;
}
