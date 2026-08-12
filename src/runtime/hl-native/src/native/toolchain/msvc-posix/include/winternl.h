/*
 * <winternl.h> for the x86_64-pc-windows-msvc target -- the NT file and
 * volume information shapes the Windows SDK omits.
 *
 * This is the one header in this directory that is not about POSIX. The
 * Windows host backend calls the NT layer directly (NtQueryInformationFile,
 * NtSetInformationFile, NtQueryDirectoryFile, NtQueryVolumeInformationFile)
 * because the Win32 wrappers cannot express what a Linux guest asks for. Those
 * calls need the FILE_*_INFORMATION structures.
 *
 * mingw-w64's <winternl.h> declares all of them. The Windows SDK's declares
 * almost none: its FILE_INFORMATION_CLASS enumeration contains exactly one
 * enumerator (FileDirectoryInformation = 1), it has no FS_INFORMATION_CLASS at
 * all, and not one of the structures below. That is a documented position --
 * Microsoft treats the NT API as internal and ships the full declarations only
 * in the WDK -- and it is the single largest non-POSIX gap between the two
 * Windows targets.
 *
 * Every definition here is transcribed from mingw-w64's <winternl.h> rather
 * than written from the online documentation. That is deliberate: these
 * structures are read and written by the kernel, so a wrong field width is
 * memory corruption and not a compile error, and the mingw-w64 lane is the
 * reference this target has to agree with byte for byte. Transcribing from the
 * header the other lane already compiles against makes agreement checkable
 * rather than asserted.
 *
 * The enumerators are #defines rather than enumerators, because the SDK has
 * already closed the FILE_INFORMATION_CLASS enumeration and C has no way to
 * extend one. The values are the same ordinals mingw-w64 assigns; they are
 * passed to a FILE_INFORMATION_CLASS parameter, and an integer converts to an
 * enumerated type implicitly in C.
 */

#ifndef HL_MSVC_POSIX_WINTERNL_H
#define HL_MSVC_POSIX_WINTERNL_H

#include_next <winternl.h>

/* ---- FILE_INFORMATION_CLASS ---------------------------------------------
 * FileDirectoryInformation (1) comes from the SDK header above. */
#define FileFullDirectoryInformation 2
#define FileBothDirectoryInformation 3
#define FileBasicInformation 4
#define FileStandardInformation 5
#define FileInternalInformation 6
#define FileEaInformation 7
#define FileAccessInformation 8
#define FileNameInformation 9
#define FileRenameInformation 10
#define FileLinkInformation 11
#define FileNamesInformation 12
#define FileDispositionInformation 13
#define FilePositionInformation 14
#define FileFullEaInformation 15
#define FileModeInformation 16
#define FileAlignmentInformation 17
#define FileAllInformation 18
#define FileAllocationInformation 19
#define FileEndOfFileInformation 20
#define FileAlternateNameInformation 21
#define FileStreamInformation 22
#define FilePipeInformation 23
#define FilePipeLocalInformation 24
#define FilePipeRemoteInformation 25
#define FileMailslotQueryInformation 26
#define FileMailslotSetInformation 27
#define FileCompressionInformation 28
#define FileObjectIdInformation 29
#define FileCompletionInformation 30
#define FileMoveClusterInformation 31
#define FileQuotaInformation 32
#define FileReparsePointInformation 33
#define FileNetworkOpenInformation 34
#define FileAttributeTagInformation 35
#define FileTrackingInformation 36
#define FileIdBothDirectoryInformation 37
#define FileIdFullDirectoryInformation 38
#define FileValidDataLengthInformation 39

/* ---- FS_INFORMATION_CLASS ------------------------------------------------
 * Absent from the SDK header entirely, so this one can be a real enumeration. */
typedef enum _HL_FS_INFORMATION_CLASS {
    FileFsVolumeInformation = 1,
    FileFsLabelInformation = 2,
    FileFsSizeInformation = 3,
    FileFsDeviceInformation = 4,
    FileFsAttributeInformation = 5,
    FileFsControlInformation = 6,
    FileFsFullSizeInformation = 7,
    FileFsObjectIdInformation = 8,
    FileFsDriverPathInformation = 9,
    FileFsVolumeFlagsInformation = 10,
    FileFsMaximumInformation = 11
} FS_INFORMATION_CLASS, *PFS_INFORMATION_CLASS;

/* ---- file information structures ---------------------------------------- */

typedef struct _FILE_BASIC_INFORMATION {
    LARGE_INTEGER CreationTime;
    LARGE_INTEGER LastAccessTime;
    LARGE_INTEGER LastWriteTime;
    LARGE_INTEGER ChangeTime;
    ULONG FileAttributes;
} FILE_BASIC_INFORMATION, *PFILE_BASIC_INFORMATION;

typedef struct _FILE_STANDARD_INFORMATION {
    LARGE_INTEGER AllocationSize;
    LARGE_INTEGER EndOfFile;
    ULONG NumberOfLinks;
    BOOLEAN DeletePending;
    BOOLEAN Directory;
} FILE_STANDARD_INFORMATION, *PFILE_STANDARD_INFORMATION;

typedef struct _FILE_INTERNAL_INFORMATION {
    LARGE_INTEGER IndexNumber;
} FILE_INTERNAL_INFORMATION, *PFILE_INTERNAL_INFORMATION;

typedef struct _FILE_EA_INFORMATION {
    ULONG EaSize;
} FILE_EA_INFORMATION, *PFILE_EA_INFORMATION;

typedef struct _FILE_ACCESS_INFORMATION {
    ACCESS_MASK AccessFlags;
} FILE_ACCESS_INFORMATION, *PFILE_ACCESS_INFORMATION;

typedef struct _FILE_POSITION_INFORMATION {
    LARGE_INTEGER CurrentByteOffset;
} FILE_POSITION_INFORMATION, *PFILE_POSITION_INFORMATION;

typedef struct _FILE_MODE_INFORMATION {
    ULONG Mode;
} FILE_MODE_INFORMATION, *PFILE_MODE_INFORMATION;

typedef struct _FILE_ALIGNMENT_INFORMATION {
    ULONG AlignmentRequirement;
} FILE_ALIGNMENT_INFORMATION, *PFILE_ALIGNMENT_INFORMATION;

typedef struct _FILE_NAME_INFORMATION {
    ULONG FileNameLength;
    WCHAR FileName[1];
} FILE_NAME_INFORMATION, *PFILE_NAME_INFORMATION;

typedef struct _FILE_LINK_INFORMATION {
    BOOLEAN ReplaceIfExists;
    HANDLE RootDirectory;
    ULONG FileNameLength;
    WCHAR FileName[1];
} FILE_LINK_INFORMATION, *PFILE_LINK_INFORMATION;

typedef struct _FILE_RENAME_INFORMATION {
    BOOLEAN ReplaceIfExists;
    HANDLE RootDirectory;
    ULONG FileNameLength;
    WCHAR FileName[1];
} FILE_RENAME_INFORMATION, *PFILE_RENAME_INFORMATION;

typedef struct _FILE_DISPOSITION_INFORMATION {
    BOOLEAN DoDeleteFile;
} FILE_DISPOSITION_INFORMATION, *PFILE_DISPOSITION_INFORMATION;

typedef struct _FILE_ALLOCATION_INFORMATION {
    LARGE_INTEGER AllocationSize;
} FILE_ALLOCATION_INFORMATION, *PFILE_ALLOCATION_INFORMATION;

typedef struct _FILE_END_OF_FILE_INFORMATION {
    LARGE_INTEGER EndOfFile;
} FILE_END_OF_FILE_INFORMATION, *PFILE_END_OF_FILE_INFORMATION;

typedef struct _FILE_NETWORK_OPEN_INFORMATION {
    LARGE_INTEGER CreationTime;
    LARGE_INTEGER LastAccessTime;
    LARGE_INTEGER LastWriteTime;
    LARGE_INTEGER ChangeTime;
    LARGE_INTEGER AllocationSize;
    LARGE_INTEGER EndOfFile;
    ULONG FileAttributes;
} FILE_NETWORK_OPEN_INFORMATION, *PFILE_NETWORK_OPEN_INFORMATION;

typedef struct _FILE_ATTRIBUTE_TAG_INFORMATION {
    ULONG FileAttributes;
    ULONG ReparseTag;
} FILE_ATTRIBUTE_TAG_INFORMATION, *PFILE_ATTRIBUTE_TAG_INFORMATION;

/* Member order matters here and is not alphabetical or obvious: the kernel
 * fills this as one contiguous block and the trailing NameInformation is
 * variable length. */
typedef struct _FILE_ALL_INFORMATION {
    FILE_BASIC_INFORMATION BasicInformation;
    FILE_STANDARD_INFORMATION StandardInformation;
    FILE_INTERNAL_INFORMATION InternalInformation;
    FILE_EA_INFORMATION EaInformation;
    FILE_ACCESS_INFORMATION AccessInformation;
    FILE_POSITION_INFORMATION PositionInformation;
    FILE_MODE_INFORMATION ModeInformation;
    FILE_ALIGNMENT_INFORMATION AlignmentInformation;
    FILE_NAME_INFORMATION NameInformation;
} FILE_ALL_INFORMATION, *PFILE_ALL_INFORMATION;

typedef struct _FILE_ID_FULL_DIR_INFORMATION {
    ULONG NextEntryOffset;
    ULONG FileIndex;
    LARGE_INTEGER CreationTime;
    LARGE_INTEGER LastAccessTime;
    LARGE_INTEGER LastWriteTime;
    LARGE_INTEGER ChangeTime;
    LARGE_INTEGER EndOfFile;
    LARGE_INTEGER AllocationSize;
    ULONG FileAttributes;
    ULONG FileNameLength;
    ULONG EaSize;
    LARGE_INTEGER FileId;
    WCHAR FileName[1];
} FILE_ID_FULL_DIR_INFORMATION, *PFILE_ID_FULL_DIR_INFORMATION;

/* ---- volume information structures --------------------------------------- */

typedef struct _FILE_FS_VOLUME_INFORMATION {
    LARGE_INTEGER VolumeCreationTime;
    ULONG VolumeSerialNumber;
    ULONG VolumeLabelLength;
    BOOLEAN SupportsObjects;
    WCHAR VolumeLabel[1];
} FILE_FS_VOLUME_INFORMATION, *PFILE_FS_VOLUME_INFORMATION;

typedef struct _FILE_FS_ATTRIBUTE_INFORMATION {
    ULONG FileSystemAttributes;
    ULONG MaximumComponentNameLength;
    ULONG FileSystemNameLength;
    WCHAR FileSystemName[1];
} FILE_FS_ATTRIBUTE_INFORMATION, *PFILE_FS_ATTRIBUTE_INFORMATION;

typedef struct _FILE_FS_FULL_SIZE_INFORMATION {
    LARGE_INTEGER TotalAllocationUnits;
    LARGE_INTEGER CallerAvailableAllocationUnits;
    LARGE_INTEGER ActualAvailableAllocationUnits;
    ULONG SectorsPerAllocationUnit;
    ULONG BytesPerSector;
} FILE_FS_FULL_SIZE_INFORMATION, *PFILE_FS_FULL_SIZE_INFORMATION;

typedef struct _FILE_FS_SIZE_INFORMATION {
    LARGE_INTEGER TotalAllocationUnits;
    LARGE_INTEGER AvailableAllocationUnits;
    ULONG SectorsPerAllocationUnit;
    ULONG BytesPerSector;
} FILE_FS_SIZE_INFORMATION, *PFILE_FS_SIZE_INFORMATION;

#endif /* HL_MSVC_POSIX_WINTERNL_H */
