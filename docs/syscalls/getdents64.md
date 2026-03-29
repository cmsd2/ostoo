# getdents64 (nr 217)

## Linux Signature

```c
int getdents64(unsigned int fd, struct linux_dirent64 *dirp, unsigned int count);
```

Where `struct linux_dirent64` is:

```c
struct linux_dirent64 {
    ino64_t        d_ino;    /* 64-bit inode number */
    off64_t        d_off;    /* 64-bit offset to next entry */
    unsigned short d_reclen; /* Size of this dirent */
    unsigned char  d_type;   /* File type */
    char           d_name[]; /* Null-terminated filename */
};
```

## Description

Reads directory entries from a directory file descriptor into a buffer. Returns the number of bytes written, or 0 when all entries have been consumed.

## Current Implementation

1. Validates that `dirp` buffer is within user address space. Returns `-EFAULT` (-14) if not.
2. Looks up `fd` in the process's file descriptor table.
3. Calls `FileHandle::getdents64()` on the handle. Only `DirHandle` implements this; other handle types return `-ENOTTY` (-25).
4. `DirHandle` maintains an internal cursor. On each call, it serializes entries starting from the cursor position into the user buffer:
   - `d_ino`: Synthetic inode number (cursor index + 1).
   - `d_off`: Index of the next entry.
   - `d_reclen`: Record length, 8-byte aligned. Computed as `8 + 8 + 2 + 1 + strlen(name) + 1`, rounded up to 8.
   - `d_type`: `DT_DIR` (4) for directories, `DT_REG` (8) for regular files.
   - `d_name`: Null-terminated filename, with zero-padding to alignment.
5. Returns total bytes written, or 0 when all entries have been read.

The directory listing is loaded entirely at `open()` time and cached in the `DirHandle`.

**Source:** `osl/src/syscalls/io.rs` — `sys_getdents64`, `osl/src/file.rs` — `DirHandle::getdents64`

## Usage from C (musl)

```c
#include <fcntl.h>
#include <sys/syscall.h>
#include <unistd.h>

int fd = open("/", O_RDONLY | O_DIRECTORY);
char buf[2048];
long nread;
while ((nread = syscall(SYS_getdents64, fd, buf, sizeof(buf))) > 0) {
    long pos = 0;
    while (pos < nread) {
        unsigned short reclen = *(unsigned short *)(buf + pos + 16);
        unsigned char  d_type = *(unsigned char *)(buf + pos + 18);
        char *name = buf + pos + 19;
        /* process entry... */
        pos += reclen;
    }
}
close(fd);
```

## Future Work

- Return proper inode numbers from the VFS.
- Support `lseek` / `rewinddir` on directory handles.
