// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT
// The census has to see what it is asked to see: N objects freed leave no trace,
// N objects kept show up as N, and N copies of an object's header (a class
// pointer and a count, where no instance was allocated) are not objects. The
// probe has private data, as every widget does, so the instance does not start
// its block. With an allocator other than glibc's the census marks its output and
// counts the copies: then N copies are expected. Run with the library preloaded
// or linked.
//
// Copies placed where an instance starts, in blocks of an instance's size, pass
// the size test, so each of the other tests gets copies only it rejects: freed
// blocks in the tcache (its key), freed blocks in a bin (PREV_INUSE), and live
// blocks whose copy has a reference count of 0. The freed ones need glibc's
// layout and are skipped without it.
#include <fcntl.h>
#include <glib-object.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

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

// The process's tcache key, learned as the census learns it: written 8 bytes into
// a block the tcache holds.
static uint64_t
tcache_key (int mem)
{
  void *probe = calloc (1, 48);
  uintptr_t address = (uintptr_t) probe;
  free (probe);
  uint64_t key = 0;
  if (pread (mem, &key, sizeof key, (off_t) (address + 8)) != sizeof key)
    return 0;
  return key;
}

// Where glibc put a freed block, read through /proc/self/mem since it is free.
// UNMARKED is a fastbin, which glibc removed in 2.43.
typedef enum { IN_TCACHE, IN_BIN, UNMARKED } Landing;

static Landing
landing (int mem, uint64_t key, gpointer block)
{
  uintptr_t user = (uintptr_t) block;
  uint64_t word = 0, header = 0, next = 0;
  if (key && pread (mem, &word, sizeof word, (off_t) (user + 8)) == sizeof word && word == key)
    return IN_TCACHE;
  if (pread (mem, &header, sizeof header, (off_t) (user - 8)) == sizeof header
      && pread (mem, &next, sizeof next, (off_t) (user - 8 + (header & ~(uint64_t) 7))) == sizeof next
      && !(next & 1))
    return IN_BIN;
  return UNMARKED;
}

int
main (int argc, char **argv)
{
  const char *dir = argc > 1 ? argv[1] : ".";
  enum { N = 500 };
  GObject *kept[N];
  // Class initialized, nothing alive.
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
  guint64 expected_copies = layout_unknown ? N : 0;

  gsize private_bytes = (gsize) ((char *) model - (char *) census_probe_get_instance_private (CENSUS_PROBE (model)));
  gsize block = private_bytes + sizeof (CensusProbe);
  guint64 freed_copies = 0;
  int landed[UNMARKED + 1] = { 0 };
  if (!layout_unknown)
    {
      int mem = open ("/proc/self/mem", O_RDONLY | O_CLOEXEC);
      if (mem < 0)
        {
          perror ("/proc/self/mem");
          return 2;
        }
      uint64_t key = tcache_key (mem);
      // Drained, the tcache takes the first copies freed and a bin the rest; the
      // guards between them stay allocated, so no two free blocks merge.
      gpointer drained[N], guarded[2 * N];
      for (int i = 0; i < N; i++)
        drained[i] = g_malloc0 (block);
      for (int i = 0; i < 2 * N; i++)
        guarded[i] = g_malloc0 (block);
      for (int i = 0; i < 2 * N; i += 2)
        {
          memcpy ((char *) guarded[i] + private_bytes, model, sizeof (GObject));
          g_free (guarded[i]);
        }
      for (int i = 0; i < 2 * N; i += 2)
        landed[landing (mem, key, guarded[i])]++;
      freed_copies = census (dir, 6) - 1;
      for (int i = 1; i < 2 * N; i += 2)
        g_free (guarded[i]);
      for (int i = 0; i < N; i++)
        g_free (drained[i]);
      close (mem);
    }

  gpointer unreferenced[N];
  for (int i = 0; i < N; i++)
    {
      unreferenced[i] = g_malloc0 (block);
      GObject *copy = (GObject *) ((char *) unreferenced[i] + private_bytes);
      memcpy (copy, model, sizeof (GObject));
      copy->ref_count = 0;
    }
  guint64 with_unreferenced = census (dir, 7) - 1;
  for (int i = 0; i < N; i++)
    g_free (unreferenced[i]);
  g_object_unref (model);

  printf ("empty=%" G_GUINT64_FORMAT " freed=%" G_GUINT64_FORMAT " kept=%" G_GUINT64_FORMAT
          " released=%" G_GUINT64_FORMAT " copies=%" G_GUINT64_FORMAT "%s\n",
          empty, after_freeing, while_kept, after_release, with_copies, layout_unknown ? " (malloc layout unknown: copies counted)" : "");
  if (layout_unknown)
    printf ("freed copies: skipped, malloc layout unknown\n");
  else
    printf ("freed copies=%" G_GUINT64_FORMAT " (tcache %d, bin %d, unmarked %d)\n", freed_copies,
            landed[IN_TCACHE], landed[IN_BIN], landed[UNMARKED]);
  printf ("unreferenced copies=%" G_GUINT64_FORMAT "\n", with_unreferenced);
  // Copies that all landed in one place test only one of the free-block tests.
  gboolean freed_ok = layout_unknown || (freed_copies == 0 && landed[IN_TCACHE] > 0 && landed[IN_BIN] > 0);
  int ok = empty == 0 && after_freeing == 0 && while_kept == N && after_release == 0
           && with_copies == expected_copies && freed_ok && with_unreferenced == 0;
  printf ("%s\n", ok ? "PASS" : "FAIL");
  return ok ? 0 : 1;
}
