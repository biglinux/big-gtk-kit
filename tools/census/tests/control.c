// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT
// The census has to see what it is asked to see: N objects freed leave no trace,
// N objects kept show up as N, and N copies of an object's header (a class
// pointer and a count, where no instance was allocated) are not objects. The
// probe has private data, as every widget does, so the instance does not start
// its block. With an allocator other than glibc's the census marks its output and
// counts the copies: then N copies are expected. Run with the library preloaded
// or linked.
#include <glib-object.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "gobject-census.h"

#define CENSUS_TYPE_PROBE (census_probe_get_type ())
G_DECLARE_FINAL_TYPE (CensusProbe, census_probe, CENSUS, PROBE, GObject)
struct _CensusProbe { GObject parent; char payload[40]; };
typedef struct { char secret[24]; } CensusProbePrivate;
G_DEFINE_FINAL_TYPE_WITH_PRIVATE (CensusProbe, census_probe, G_TYPE_OBJECT)
static void census_probe_class_init (CensusProbeClass *klass) { (void) klass; }
static void census_probe_init (CensusProbe *self) { (void) self; }

static gboolean layout_unknown;

static guint64
count_in (const char *path, const char *type)
{
  FILE *in = fopen (path, "r");
  if (!in)
    return (guint64) -1;
  char line[512];
  guint64 found = 0;
  while (fgets (line, sizeof line, in))
    {
      guint64 n;
      char name[256];
      if (line[0] == '#')
        layout_unknown = TRUE;
      else if (sscanf (line, "%" G_GUINT64_FORMAT " %255s", &n, name) == 2 && g_strcmp0 (name, type) == 0)
        found = n;
    }
  fclose (in);
  return found;
}

static guint64
census (const char *dir, int serial)
{
  char *path = g_strdup_printf ("%s/census.%d", dir, serial);
  if (!gobject_census_write (path))
    {
      fprintf (stderr, "census could not be written to %s\n", path);
      exit (2);
    }
  guint64 n = count_in (path, "CensusProbe");
  g_free (path);
  return n;
}

int
main (int argc, char **argv)
{
  const char *dir = argc > 1 ? argv[1] : ".";
  enum { N = 500 };
  GObject *kept[N];
  // Class initialised, nothing alive.
  g_object_unref (g_object_new (CENSUS_TYPE_PROBE, NULL));
  guint64 empty = census (dir, 1);
  for (int i = 0; i < N; i++)
    g_object_unref (g_object_new (CENSUS_TYPE_PROBE, NULL));
  guint64 after_freeing = census (dir, 2);
  for (int i = 0; i < N; i++)
    kept[i] = g_object_new (CENSUS_TYPE_PROBE, NULL);
  guint64 while_kept = census (dir, 3);
  for (int i = 0; i < N; i++)
    g_object_unref (kept[i]);
  guint64 after_release = census (dir, 4);
  GObject *model = g_object_new (CENSUS_TYPE_PROBE, NULL);
  gpointer copies[N];
  for (int i = 0; i < N; i++)
    {
      copies[i] = g_malloc0 (256);
      memcpy (copies[i], model, sizeof (GObject));
    }
  guint64 with_copies = census (dir, 5) - 1;
  for (int i = 0; i < N; i++)
    g_free (copies[i]);
  g_object_unref (model);
  guint64 expected_copies = layout_unknown ? N : 0;
  printf ("empty=%" G_GUINT64_FORMAT " freed=%" G_GUINT64_FORMAT " kept=%" G_GUINT64_FORMAT
          " released=%" G_GUINT64_FORMAT " copies=%" G_GUINT64_FORMAT "%s\n",
          empty, after_freeing, while_kept, after_release, with_copies, layout_unknown ? " (malloc layout unknown: copies counted)" : "");
  int ok = empty == 0 && after_freeing == 0 && while_kept == N && after_release == 0
           && with_copies == expected_copies;
  printf ("%s\n", ok ? "PASS" : "FAIL");
  return ok ? 0 : 1;
}
