// How demirror talks to its display driver: IOCTLs on the driver's device interface.
// Mirrored in crates/vmon-server/src/source/idd.rs; change both together.

#pragma once

// {b3fbedff-97db-4045-81af-e2cf872da1da}
static const GUID DEMIRROR_IDD_INTERFACE =
    {0xb3fbedff, 0x97db, 0x4045, {0x81, 0xaf, 0xe2, 0xcf, 0x87, 0x2d, 0xa1, 0xda}};

#define DEMIRROR_IDD_VERSION 1

// CTL_CODE(FILE_DEVICE_UNKNOWN, 0x800 + n, METHOD_BUFFERED, FILE_ANY_ACCESS)
// Out: UINT32 version.
#define IOCTL_DEMIRROR_VERSION 0x222000
// In: DEMIRROR_SIZE. Out: UINT32 id of the new monitor.
#define IOCTL_DEMIRROR_ADD 0x222004
// In: UINT32 id.
#define IOCTL_DEMIRROR_REMOVE 0x222008
// In: UINT32 id. A monitor not pinged for DEMIRROR_PING_TIMEOUT_MS is removed, so monitors
// don't outlive the program that added them.
#define IOCTL_DEMIRROR_PING 0x22200C
// In: DEMIRROR_RESIZE.
#define IOCTL_DEMIRROR_RESIZE 0x222010

#define DEMIRROR_PING_TIMEOUT_MS 5000
#define DEMIRROR_MAX_MONITORS 8
#define DEMIRROR_MAX_SIDE 8192

#pragma pack(push, 4)
struct DEMIRROR_SIZE {
    UINT32 Width;
    UINT32 Height;
};

struct DEMIRROR_RESIZE {
    UINT32 Id;
    UINT32 Width;
    UINT32 Height;
};
#pragma pack(pop)
