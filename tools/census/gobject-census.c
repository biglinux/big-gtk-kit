// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT
//
// gobject-census: how many instances of every GType are alive in this process.
//
// Preloaded into a GLib/GTK program:
//
//   GOBJECT_CENSUS_OUT=/some/dir/census LD_PRELOAD=.../libgobject-census.so program
//   kill -USR2 <pid>          # writes /some/dir/census.<pid>.<n>, n = 1, 2, 3...
//
// The environment is left alone, so a wrapper in between (a shell, a session
// script) passes it on; each process that loads it names its files by its pid.
//
// or called directly: `gobject_census_write (path)` (declared in gobject-census.h).
//
// Why not g_type_get_instance_count(): it only counts when GLib is built with
// G_ENABLE_DEBUG, and distributions do not build it that way. Why not a debugger:
// reading another process needs ptrace, which yama restricts to its ancestors.
// So this counts from inside: every GTypeInstance begins with a pointer to its
// class, and the class of every initialised type is known, so the process's own
// writable anonymous memory is scanned for those pointers.
//
// A freed block keeps the class pointer it had, so a GObject is only counted when
// the reference count after the pointer is plausible (1..1e6; a finalised object
// leaves 0 there) and, when the allocator is glibc's, when the pointer starts an
// instance at the right place in a malloc block that is in use. The last test
// exists because copies of a class pointer outlive their instances: a dialog
// opened and freed six times left six records holding its class pointer and
// stack addresses, and they were counted as six live dialogs. With another
// allocator that test is off and the census says so in its first line. Counts are
// approximate; what is meaningful is the DIFFERENCE between two censuses of the
// same process.
#define _GNU_SOURCE
#include <fcntl.h>
#include <glib-object.h>
#include <glib-unix.h>
#include <glib.h>
#include <dlfcn.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#include "gobject-census.h"

// Malloc headers, 16-byte instance alignment and the scan's word size are those of
// a 64-bit glibc.
G_STATIC_ASSERT (sizeof (void *) == 8);

typedef struct {
  uintptr_t klass;
  GType type;
  gboolean is_object;
  gboolean is_texture;
  guint instance_size;
  gint private_bytes;  // learnt from the first block found; -1 until then
  guint64 count;
} Entry;

// The table and the read buffer live in mmaps of their own, left out of the scan,
// so the census never counts its own copy of the class pointers.
typedef struct {
  Entry *entries;
  gsize len;
  gsize cap;
  gsize bytes;
} Table;

#define CHUNK (8u << 20)

static int
compare_entries (const void *a, const void *b)
{
  uintptr_t x = ((const Entry *) a)->klass, y = ((const Entry *) b)->klass;
  return x < y ? -1 : x > y;
}

static gboolean
table_push (Table *table, Entry entry)
{
  if (table->len == table->cap)
    {
      gsize cap = table->cap ? table->cap * 2 : 4096;
      gsize bytes = cap * sizeof (Entry);
      Entry *grown = mmap (NULL, bytes, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
      if (grown == MAP_FAILED)
        return FALSE;
      if (table->entries)
        {
          memcpy (grown, table->entries, table->len * sizeof (Entry));
          munmap (table->entries, table->bytes);
        }
      table->entries = grown;
      table->cap = cap;
      table->bytes = bytes;
    }
  table->entries[table->len++] = entry;
  return TRUE;
}

static void
collect (Table *table, GType type, GType texture_type)
{
  if (G_TYPE_IS_CLASSED (type) && G_TYPE_IS_INSTANTIATABLE (type))
    {
      gpointer klass = g_type_class_peek (type);
      if (klass)
        {
          Entry entry = { (uintptr_t) klass, type, g_type_is_a (type, G_TYPE_OBJECT),
                          texture_type && g_type_is_a (type, texture_type), 0, -1, 0 };
          GTypeQuery query;
          g_type_query (type, &query);
          entry.instance_size = query.instance_size;
          table_push (table, entry);
        }
    }
  guint n = 0;
  GType *children = g_type_children (type, &n);
  for (guint i = 0; i < n; i++)
    collect (table, children[i], texture_type);
  g_free (children);
}

static Entry *
lookup (Table *table, uintptr_t word)
{
  gsize lo = 0, hi = table->len;
  while (lo < hi)
    {
      gsize mid = (lo + hi) / 2;
      if (table->entries[mid].klass < word)
        lo = mid + 1;
      else
        hi = mid;
    }
  return lo < table->len && table->entries[lo].klass == word ? &table->entries[lo] : NULL;
}

typedef struct {
  uintptr_t *addresses;
  gsize len;
  gsize cap;
  gsize bytes;
} Found;

static void
found_push (Found *found, uintptr_t address)
{
  if (found->len == found->cap)
    {
      gsize cap = found->cap ? found->cap * 2 : 1024;
      gsize bytes = cap * sizeof (uintptr_t);
      uintptr_t *grown = mmap (NULL, bytes, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
      if (grown == MAP_FAILED)
        return;
      if (found->addresses)
        {
          memcpy (grown, found->addresses, found->len * sizeof (uintptr_t));
          munmap (found->addresses, found->bytes);
        }
      found->addresses = grown;
      found->cap = cap;
      found->bytes = bytes;
    }
  found->addresses[found->len++] = address;
}

static gboolean
excluded (uintptr_t lo, uintptr_t hi, const void *block, gsize bytes)
{
  uintptr_t start = (uintptr_t) block;
  return block && start < hi && start + bytes > lo;
}

// glibc marks a free block in two ways. A block in a thread cache has, 8 bytes
// into it, the process's tcache key; any other free block has the PREV_INUSE bit
// clear in the header of the block after it. The key is learnt by freeing a block
// of our own; 0 when that block did not go to the thread cache, and then the
// first test is skipped. Anything that does not look like a malloc header keeps
// the object counted.
static uintptr_t tcache_key;

static void
learn_tcache_key (int mem)
{
  void *probe = calloc (1, 48);
  if (!probe)
    return;
  uintptr_t address = (uintptr_t) probe;
  free (probe);
  // Read through /proc/self/mem: the block is free now, which is the point.
  if (pread (mem, &tcache_key, sizeof tcache_key, (off_t) (address + 8)) != sizeof tcache_key)
    tcache_key = 0;
}

// GLib allocates an instance and its private data with one malloc, private data
// first, so the block starts some bytes before the instance, and its header holds
// exactly the size glibc makes of (private + instance) bytes. That equation finds
// the header; the distance is the same for every instance of a type, so it is
// searched for once and then only checked. GLib cannot be asked for it:
// g_type_class_get_instance_private_offset() aborts on a type that adds no private
// data of its own. A copy of an instance in a block of exactly its size therefore
// still passes for a type without private data.
#define MAX_PRIVATE 16384
#define WINDOW_WORDS (MAX_PRIVATE / 8 + 1)

// Whether the blocks are glibc's, checked on an object made for the purpose. When
// they are, a class pointer that does not start an instance in a block of the
// right size is not an instance. When they are not, nothing is rejected on it.
static gboolean malloc_layout_known;

static uint64_t
chunk_size (gsize request)
{
  uint64_t size = (request + 8 + 15) & ~(uint64_t) 15;
  return size < 32 ? 32 : size;
}

// malloc hands over a block 16 bytes larger when splitting it would leave a
// remainder too small to be a block.
static gboolean
header_fits (uint64_t header, gsize private_bytes, guint instance_size)
{
  uint64_t size = header & ~(uint64_t) 7, wanted = chunk_size (private_bytes + instance_size);
  return (size == wanted || size == wanted + 16) && !(header & 2);
}

// TRUE when `instance` is not a live instance of `entry`'s type: (glibc blocks
// only) it is not where an instance of that type starts, or its block is free.
// `window` is scratch space for the search, outside the scanned memory.
static gboolean
not_an_instance (int mem, uintptr_t instance, Entry *entry, uint64_t *window)
{
  if (!malloc_layout_known || !entry->instance_size)
    return FALSE;
  uint64_t header;
  gsize private_bytes = (gsize) entry->private_bytes;
  if (entry->private_bytes < 0
      || pread (mem, &header, sizeof header, (off_t) (instance - private_bytes - 8)) != sizeof header
      || !header_fits (header, private_bytes, entry->instance_size))
    {
      gsize bytes = WINDOW_WORDS * sizeof (uint64_t);
      if (pread (mem, window, bytes, (off_t) (instance - bytes)) != (ssize_t) bytes)
        return FALSE;
      gboolean found = FALSE;
      // window[WINDOW_WORDS - 1] is the word just before the instance: private size 0.
      for (gsize back = 0; back < MAX_PRIVATE && !found; back += 16)
        if (header_fits (window[WINDOW_WORDS - 1 - back / 8], back, entry->instance_size))
          {
            private_bytes = back;
            header = window[WINDOW_WORDS - 1 - back / 8];
            found = TRUE;
          }
      if (!found)
        return TRUE;
      entry->private_bytes = (gint) private_bytes;
    }
  uintptr_t user = instance - private_bytes;
  uint64_t size = header & ~(uint64_t) 7;
  uint64_t key;
  if (tcache_key && pread (mem, &key, sizeof key, (off_t) (user + 8)) == sizeof key
      && key == tcache_key)
    return TRUE;
  uint64_t next;
  if (pread (mem, &next, sizeof next, (off_t) (user - 8 + size)) != sizeof next)
    return FALSE;
  return !(next & 1);
}

// FALSE when the process's memory could not be read at all.
static gboolean
scan (Table *table, Found *textures)
{
  int mem = open ("/proc/self/mem", O_RDONLY | O_CLOEXEC);
  FILE *maps = fopen ("/proc/self/maps", "re");
  // The read buffer, then the search window: one mapping, left out of the scan.
  gsize mapped = CHUNK + WINDOW_WORDS * sizeof (uint64_t);
  uint64_t *buffer = mmap (NULL, mapped, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  gboolean ok = mem >= 0 && maps && buffer != MAP_FAILED;
  if (!ok)
    goto out;
  if (!tcache_key)
    learn_tcache_key (mem);
  {
    GObject *known = g_object_new (G_TYPE_OBJECT, NULL);
    uint64_t header;
    malloc_layout_known = pread (mem, &header, sizeof header, (off_t) ((uintptr_t) known - 8)) == sizeof header
                          && header_fits (header, 0, sizeof (GObject));
    g_object_unref (known);
  }
  gsize page = (gsize) sysconf (_SC_PAGESIZE);
  unsigned char resident[CHUNK / 4096];
  uintptr_t lowest = table->entries[0].klass, highest = table->entries[table->len - 1].klass;
  char line[4096];
  while (fgets (line, sizeof line, maps))
    {
      unsigned long lo, hi;
      char perms[8] = "";
      char path[4096] = "";
      if (sscanf (line, "%lx-%lx %7s %*s %*s %*s %4095[^\n]", &lo, &hi, perms, path) < 3)
        continue;
      if (perms[0] != 'r' || perms[1] != 'w')
        continue;
      // Anonymous memory and the heap: where instances live. Thread arenas and
      // thread stacks have no name, so an unnamed region is never skipped; some
      // allocators name theirs (`[anon:mimalloc]`, glibc's decorate_maps).
      char *name = path;
      while (*name == ' ')
        name++;
      if (*name && strcmp (name, "[heap]") != 0 && strncmp (name, "[anon:", 6) != 0)
        continue;
      if (excluded (lo, hi, table->entries, table->bytes) || excluded (lo, hi, buffer, mapped)
          || excluded (lo, hi, textures->addresses, textures->bytes))
        continue;
      for (uintptr_t at = lo; at < hi; at += CHUNK)
        {
          gsize pages = MIN ((uintptr_t) CHUNK, hi - at) / page;
          // Only resident pages are read: reading never-touched ones of a huge
          // reservation (ASan's 14 TiB shadow) never ends. Swapped-out instances
          // are missed, which a census under memory pressure has to allow for.
          if (mincore ((void *) at, pages * page, resident) != 0)
            continue;
          for (gsize first = 0, last; first < pages; first = last)
            {
              for (last = first; last < pages && (resident[last] & 1); last++)
                ;
              if (last == first)
                {
                  last++;
                  continue;
                }
              uintptr_t start = at + first * page;
              ssize_t got = pread (mem, buffer, (last - first) * page, (off_t) start);
              gsize words = got > 0 ? (gsize) got / sizeof (uint64_t) : 0;
              // Instances start 16-byte aligned (malloc and GLib's private offset).
              for (gsize i = 0; i + 1 < words; i += 2)
                {
                  uintptr_t word = (uintptr_t) buffer[i];
                  if (word < lowest || word > highest)
                    continue;
                  Entry *entry = lookup (table, word);
                  if (!entry)
                    continue;
                  uintptr_t instance = start + i * sizeof (uint64_t);
                  if (entry->is_object)
                    {
                      guint32 refs = (guint32) (buffer[i + 1] & 0xffffffffu);
                      if (refs < 1 || refs > 1000000)
                        continue;
                      if (not_an_instance (mem, instance, entry, buffer + CHUNK / sizeof (uint64_t)))
                        continue;
                    }
                  entry->count++;
                  if (entry->is_texture)
                    found_push (textures, instance);
                }
            }
        }
    }
out:
  if (buffer != MAP_FAILED)
    munmap (buffer, mapped);
  if (mem >= 0)
    close (mem);
  if (maps)
    fclose (maps);
  return ok;
}

typedef int (*SizeFn) (gpointer);

static void
write_textures (FILE *out, Found *textures, GType texture_type)
{
  SizeFn width = (SizeFn) dlsym (RTLD_DEFAULT, "gdk_texture_get_width");
  SizeFn height = (SizeFn) dlsym (RTLD_DEFAULT, "gdk_texture_get_height");
  if (!width || !height || !textures->len)
    return;
  GHashTable *sizes = g_hash_table_new_full (g_str_hash, g_str_equal, g_free, NULL);
  for (gsize i = 0; i < textures->len; i++)
    {
      gpointer instance = (gpointer) textures->addresses[i];
      // Only call into GDK on something that really is a texture right now.
      if (!g_type_check_instance_is_a (instance, texture_type))
        continue;
      char *key = g_strdup_printf ("%dx%d", width (instance), height (instance));
      guint n = GPOINTER_TO_UINT (g_hash_table_lookup (sizes, key));
      g_hash_table_replace (sizes, key, GUINT_TO_POINTER (n + 1));
    }
  GHashTableIter iter;
  gpointer key, value;
  g_hash_table_iter_init (&iter, sizes);
  while (g_hash_table_iter_next (&iter, &key, &value))
    fprintf (out, "%u texture %s\n", GPOINTER_TO_UINT (value), (const char *) key);
  g_hash_table_unref (sizes);
}

gboolean
gobject_census_write (const char *path)
{
  typedef GType (*TypeFn) (void);
  TypeFn texture_get_type = (TypeFn) dlsym (RTLD_DEFAULT, "gdk_texture_get_type");
  GType texture_type = texture_get_type ? texture_get_type () : 0;

  Table table = { 0 };
  GType last = g_type_fundamental_next ();
  for (GType f = G_TYPE_MAKE_FUNDAMENTAL (1); f < last; f += G_TYPE_MAKE_FUNDAMENTAL (1))
    if (g_type_name (f))
      collect (&table, f, texture_type);
  if (!table.len)
    return FALSE;
  qsort (table.entries, table.len, sizeof (Entry), compare_entries);

  Found textures = { 0 };
  gboolean ok = scan (&table, &textures);

  char *partial = g_strdup_printf ("%s.partial", path);
  FILE *out = ok ? fopen (partial, "we") : NULL;
  ok = out != NULL;
  if (out)
    {
      if (!malloc_layout_known)
        fputs ("# malloc layout unknown: copies of an instance header count as instances\n", out);
      for (gsize i = 0; i < table.len; i++)
        if (table.entries[i].count)
          fprintf (out, "%" G_GUINT64_FORMAT " %s\n", table.entries[i].count,
                   g_type_name (table.entries[i].type));
      if (texture_type)
        write_textures (out, &textures, texture_type);
      ok = fclose (out) == 0 && rename (partial, path) == 0;
    }
  g_free (partial);
  munmap (table.entries, table.bytes);
  if (textures.addresses)
    munmap (textures.addresses, textures.bytes);
  return ok;
}

static char *census_prefix;
static guint census_serial;

static gboolean
on_signal (gpointer data)
{
  (void) data;
  char *path = g_strdup_printf ("%s.%d.%u", census_prefix, (int) getpid (), ++census_serial);
  if (!gobject_census_write (path))
    g_warning ("gobject-census: could not write %s", path);
  g_free (path);
  return G_SOURCE_CONTINUE;
}

__attribute__ ((constructor)) static void
install (void)
{
  const char *prefix = getenv ("GOBJECT_CENSUS_OUT");
  if (!prefix || !*prefix)
    return;
  census_prefix = g_strdup (prefix);
  // Dispatched by the default main context, on the main thread, between events:
  // never GLib inside a raw signal handler.
  g_unix_signal_add (SIGUSR2, on_signal, NULL);
}
