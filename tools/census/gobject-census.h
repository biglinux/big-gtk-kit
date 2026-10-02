// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT
#pragma once

#include <glib.h>

G_BEGIN_DECLS

// Write "count TypeName" for every GType with live instances to `path` (via
// `path.partial` and a rename, so a reader never sees half a census), plus
// "count texture WxH" lines when GDK is loaded. A first line starting with `#`
// says the allocator is not glibc's, so header copies are counted too. Main thread
// only. Returns FALSE when nothing could be written or memory could not be read.
gboolean gobject_census_write (const char *path);

G_END_DECLS
