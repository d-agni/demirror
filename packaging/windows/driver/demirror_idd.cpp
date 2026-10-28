// demirror's display driver for Windows: an Indirect Display (IddCx) driver.
//
// demirror asks it, through IOCTLs (protocol.h), for a monitor of the viewing device's size.
// The driver plugs one in, Windows extends the desktop onto it like onto any new monitor, and
// demirror captures it like any other screen. The driver itself only keeps the monitor's
// frames flowing; the pixels go to the viewer through demirror.
//
// Monitors are made to measure: each offers exactly the size it was asked for. They go away
// when demirror removes them or stops pinging them (it quit or crashed).
//
// The IddCx parts follow the shape of Microsoft's Indirect Display sample driver (MIT).

#define NOMINMAX
#define WIN32_NO_STATUS
#include <windows.h>
#undef WIN32_NO_STATUS
#include <ntstatus.h>
#include <bugcodes.h>
#include <wudfwdm.h>
#include <wdf.h>
#include <IddCx.h>
#include <dxgi1_5.h>
#include <d3d11_2.h>
#include <wrl/client.h>

#include <memory>
#include <mutex>
#include <vector>

#include "protocol.h"

using Microsoft::WRL::ComPtr;

namespace {

const UINT REFRESH_HZ = 60;

// Keeps a monitor's swap chain moving: Windows waits for each frame to be taken before it
// draws the next, so without this the desktop on the monitor would stop updating.
class Worker {
public:
    Worker(IDDCX_SWAPCHAIN chain, HANDLE available, ComPtr<ID3D11Device> device)
        : m_chain(chain), m_available(available), m_device(std::move(device)) {
        m_stop = CreateEventW(nullptr, TRUE, FALSE, nullptr);
        m_thread = CreateThread(nullptr, 0, Run, this, 0, nullptr);
    }

    ~Worker() {
        SetEvent(m_stop);
        if (m_thread) {
            WaitForSingleObject(m_thread, INFINITE);
            CloseHandle(m_thread);
        }
        CloseHandle(m_stop);
    }

    Worker(const Worker&) = delete;
    Worker& operator=(const Worker&) = delete;

private:
    static DWORD CALLBACK Run(LPVOID self) {
        static_cast<Worker*>(self)->Loop();
        return 0;
    }

    void Loop() {
        ComPtr<IDXGIDevice> dxgi;
        if (SUCCEEDED(m_device.As(&dxgi))) {
            IDARG_IN_SWAPCHAINSETDEVICE set = {};
            set.pDevice = dxgi.Get();
            if (SUCCEEDED(IddCxSwapChainSetDevice(m_chain, &set))) {
                Frames();
            }
        }
        // Done with this swap chain (abandoned, or stopping): deleting it lets Windows make
        // a new one if the monitor is still in use.
        WdfObjectDelete((WDFOBJECT)m_chain);
    }

    void Frames() {
        for (;;) {
            IDARG_OUT_RELEASEANDACQUIREBUFFER buffer = {};
            HRESULT hr = IddCxSwapChainReleaseAndAcquireBuffer(m_chain, &buffer);
            if (hr == E_PENDING) {
                HANDLE events[] = {m_available, m_stop};
                DWORD woke = WaitForMultipleObjects(2, events, FALSE, 16);
                if (woke == WAIT_OBJECT_0 || woke == WAIT_TIMEOUT) {
                    continue;
                }
                return;
            }
            if (FAILED(hr)) {
                return; // abandoned: a mode change, or the monitor is gone
            }
            // demirror reads the desktop back through Windows, so the frame isn't needed here.
            if (buffer.MetaData.pSurface) {
                buffer.MetaData.pSurface->Release();
            }
            if (FAILED(IddCxSwapChainFinishedProcessingFrame(m_chain))) {
                return;
            }
            if (WaitForSingleObject(m_stop, 0) == WAIT_OBJECT_0) {
                return;
            }
        }
    }

    IDDCX_SWAPCHAIN m_chain;
    HANDLE m_available;
    ComPtr<ID3D11Device> m_device;
    HANDLE m_stop = nullptr;
    HANDLE m_thread = nullptr;
};

struct Monitor {
    UINT32 id = 0;
    UINT connector = 0;
    UINT32 width = 0;
    UINT32 height = 0;
    ULONGLONG pinged = 0;
    IDDCX_MONITOR object = nullptr;
    BYTE edid[128] = {};
    std::unique_ptr<Worker> worker;
};

// Everything below is guarded by `g_lock`. IddCx calls back into the driver from inside its
// own functions, so the lock is never held while calling IddCx.
std::mutex g_lock;
IDDCX_ADAPTER g_adapter = nullptr;
bool g_ready = false;
std::vector<std::unique_ptr<Monitor>> g_monitors;
UINT32 g_nextId = 1;
WDFTIMER g_watchdog = nullptr;

Monitor* ByObject(IDDCX_MONITOR object) {
    for (auto& m : g_monitors) {
        if (m->object == object) {
            return m.get();
        }
    }
    return nullptr;
}

Monitor* ByConnector(UINT connector) {
    for (auto& m : g_monitors) {
        if (m->connector == connector) {
            return m.get();
        }
    }
    return nullptr;
}

std::unique_ptr<Monitor> Take(UINT32 id) {
    for (auto it = g_monitors.begin(); it != g_monitors.end(); ++it) {
        if ((*it)->id == id) {
            auto m = std::move(*it);
            g_monitors.erase(it);
            return m;
        }
    }
    return nullptr;
}

// Stops a monitor's frames, then unplugs it. Called without the lock.
void Unplug(std::unique_ptr<Monitor> m) {
    m->worker.reset();
    if (m->object) {
        IddCxMonitorDeparture(m->object);
    }
}

// An EDID that names the monitor "demirror" and tells Windows which connector it's on (the
// serial number), so each connector keeps its place in the display arrangement. Its sizes
// aren't used: the modes come from `Modes`.
void MakeEdid(BYTE edid[128], UINT serial) {
    static const BYTE head[] = {0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00,
                                0x11, 0xB2, // manufacturer "DMR"
                                0x01, 0x00}; // product
    static const BYTE info[] = {0x00, 36,   // week, year (2026)
                                0x01, 0x03, // EDID 1.3
                                0x80,       // digital input
                                0x00, 0x00, // size unknown
                                0x78,       // gamma 2.2
                                0x06,       // sRGB, preferred timing first
                                0xEE, 0x91, 0xA3, 0x54, 0x4C, 0x99, 0x26, 0x0F, 0x50, 0x54}; // sRGB
    // 1920x1080 at 60 Hz, a timing every system knows.
    static const BYTE timing[] = {0x02, 0x3A, 0x80, 0x18, 0x71, 0x38, 0x2D, 0x40, 0x58,
                                  0x2C, 0x45, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1E};
    static const BYTE name[] = {0x00, 0x00, 0x00, 0xFC, 0x00, 'd', 'e', 'm', 'i',
                                'r',  'r',  'o',  'r',  0x0A, ' ', ' ', ' ', ' '};
    static const BYTE dummy[] = {0x00, 0x00, 0x00, 0x10, 0x00};

    ZeroMemory(edid, 128);
    memcpy(edid, head, sizeof(head));
    memcpy(edid + 12, &serial, 4);
    memcpy(edid + 16, info, sizeof(info));
    memset(edid + 38, 0x01, 16); // no standard timings
    memcpy(edid + 54, timing, sizeof(timing));
    memcpy(edid + 72, name, sizeof(name));
    memcpy(edid + 90, dummy, sizeof(dummy));
    memcpy(edid + 108, dummy, sizeof(dummy));
    BYTE sum = 0;
    for (int i = 0; i < 127; i++) {
        sum += edid[i];
    }
    edid[127] = (BYTE)(256 - sum);
}

void Signal(DISPLAYCONFIG_VIDEO_SIGNAL_INFO& s, UINT32 width, UINT32 height, bool monitorMode) {
    s = {};
    s.totalSize.cx = s.activeSize.cx = width;
    s.totalSize.cy = s.activeSize.cy = height;
    s.AdditionalSignalInfo.vSyncFreqDivider = monitorMode ? 0 : 1;
    s.AdditionalSignalInfo.videoStandard = 255;
    s.vSyncFreq.Numerator = REFRESH_HZ;
    s.vSyncFreq.Denominator = 1;
    s.hSyncFreq.Numerator = REFRESH_HZ * height;
    s.hSyncFreq.Denominator = 1;
    s.scanLineOrdering = DISPLAYCONFIG_SCANLINE_ORDERING_PROGRESSIVE;
    s.pixelRate = (UINT64)REFRESH_HZ * width * height;
}

void TargetMode(IDDCX_TARGET_MODE& mode, UINT32 width, UINT32 height) {
    mode = {};
    mode.Size = sizeof(mode);
    Signal(mode.TargetVideoSignalInfo.targetVideoSignalInfo, width, height, false);
}

bool SizeOk(UINT32 width, UINT32 height) {
    return width >= 64 && height >= 64 && width <= DEMIRROR_MAX_SIDE && height <= DEMIRROR_MAX_SIDE;
}

NTSTATUS AddMonitor(const DEMIRROR_SIZE& size, UINT32* id) {
    if (!SizeOk(size.Width, size.Height)) {
        return STATUS_INVALID_PARAMETER;
    }
    IDDCX_ADAPTER adapter;
    Monitor* m;
    {
        std::lock_guard<std::mutex> lock(g_lock);
        if (!g_ready) {
            return STATUS_DEVICE_NOT_READY;
        }
        UINT connector = 0;
        while (connector < DEMIRROR_MAX_MONITORS && ByConnector(connector)) {
            connector++;
        }
        if (connector == DEMIRROR_MAX_MONITORS) {
            return STATUS_INSUFFICIENT_RESOURCES;
        }
        auto added = std::make_unique<Monitor>();
        added->id = g_nextId++;
        added->connector = connector;
        added->width = size.Width;
        added->height = size.Height;
        added->pinged = GetTickCount64();
        MakeEdid(added->edid, connector + 1);
        m = added.get();
        g_monitors.push_back(std::move(added));
        adapter = g_adapter;
    }
    // `m` stays valid: nothing removes a monitor before its id is handed out.
    const UINT32 newId = m->id;

    IDDCX_MONITOR_INFO info = {};
    info.Size = sizeof(info);
    info.MonitorType = DISPLAYCONFIG_OUTPUT_TECHNOLOGY_HDMI;
    info.ConnectorIndex = m->connector;
    info.MonitorDescription.Size = sizeof(info.MonitorDescription);
    info.MonitorDescription.Type = IDDCX_MONITOR_DESCRIPTION_TYPE_EDID;
    info.MonitorDescription.DataSize = sizeof(m->edid);
    info.MonitorDescription.pData = m->edid;
    // {65db5555-4c65-4d1b-b6cf-fa7a2bd997xx}, xx the connector.
    info.MonitorContainerId = {0x65db5555, 0x4c65, 0x4d1b, {0xb6, 0xcf, 0xfa, 0x7a, 0x2b, 0xd9, 0x97, (BYTE)m->connector}};

    WDF_OBJECT_ATTRIBUTES attributes;
    WDF_OBJECT_ATTRIBUTES_INIT(&attributes);
    IDARG_IN_MONITORCREATE create = {};
    create.ObjectAttributes = &attributes;
    create.pMonitorInfo = &info;
    IDARG_OUT_MONITORCREATE created = {};
    NTSTATUS status = IddCxMonitorCreate(adapter, &create, &created);
    if (!NT_SUCCESS(status)) {
        std::lock_guard<std::mutex> lock(g_lock);
        Take(newId);
        return status;
    }
    {
        std::lock_guard<std::mutex> lock(g_lock);
        m->object = created.MonitorObject;
    }
    IDARG_OUT_MONITORARRIVAL arrived = {};
    status = IddCxMonitorArrival(created.MonitorObject, &arrived);
    if (!NT_SUCCESS(status)) {
        {
            std::lock_guard<std::mutex> lock(g_lock);
            Take(newId);
        }
        WdfObjectDelete((WDFOBJECT)created.MonitorObject);
        return status;
    }
    *id = newId;
    return STATUS_SUCCESS;
}

NTSTATUS RemoveMonitor(UINT32 id) {
    std::unique_ptr<Monitor> m;
    {
        std::lock_guard<std::mutex> lock(g_lock);
        m = Take(id);
    }
    if (!m) {
        return STATUS_NOT_FOUND;
    }
    Unplug(std::move(m));
    return STATUS_SUCCESS;
}

NTSTATUS PingMonitor(UINT32 id) {
    std::lock_guard<std::mutex> lock(g_lock);
    for (auto& m : g_monitors) {
        if (m->id == id) {
            m->pinged = GetTickCount64();
            return STATUS_SUCCESS;
        }
    }
    return STATUS_NOT_FOUND;
}

// Offers the new size as the monitor's only mode; demirror then asks Windows to switch to it.
NTSTATUS ResizeMonitor(const DEMIRROR_RESIZE& resize) {
    if (!SizeOk(resize.Width, resize.Height)) {
        return STATUS_INVALID_PARAMETER;
    }
    IDDCX_MONITOR object = nullptr;
    {
        std::lock_guard<std::mutex> lock(g_lock);
        for (auto& m : g_monitors) {
            if (m->id == resize.Id) {
                m->width = resize.Width;
                m->height = resize.Height;
                object = m->object;
            }
        }
    }
    if (!object) {
        return STATUS_NOT_FOUND;
    }
    IDDCX_TARGET_MODE mode;
    TargetMode(mode, resize.Width, resize.Height);
    IDARG_IN_UPDATEMODES update = {};
    update.Reason = IDDCX_UPDATE_REASON_OTHER;
    update.TargetModeCount = 1;
    update.pTargetModes = &mode;
    return IddCxMonitorUpdateModes(object, &update);
}

// Removes the monitors of a demirror that stopped pinging (it quit or crashed).
VOID EvtWatchdog(WDFTIMER) {
    std::vector<std::unique_ptr<Monitor>> stale;
    {
        std::lock_guard<std::mutex> lock(g_lock);
        const ULONGLONG now = GetTickCount64();
        for (auto it = g_monitors.begin(); it != g_monitors.end();) {
            if ((*it)->object && now - (*it)->pinged > DEMIRROR_PING_TIMEOUT_MS) {
                stale.push_back(std::move(*it));
                it = g_monitors.erase(it);
            } else {
                ++it;
            }
        }
    }
    for (auto& m : stale) {
        Unplug(std::move(m));
    }
}

template <typename T>
NTSTATUS Input(WDFREQUEST request, T** in) {
    return WdfRequestRetrieveInputBuffer(request, sizeof(T), (PVOID*)in, nullptr);
}

VOID EvtIoControl(WDFDEVICE, WDFREQUEST request, size_t, size_t, ULONG code) {
    NTSTATUS status;
    size_t written = 0;
    switch (code) {
    case IOCTL_DEMIRROR_VERSION: {
        UINT32* out;
        status = WdfRequestRetrieveOutputBuffer(request, sizeof(UINT32), (PVOID*)&out, nullptr);
        if (NT_SUCCESS(status)) {
            *out = DEMIRROR_IDD_VERSION;
            written = sizeof(UINT32);
        }
        break;
    }
    case IOCTL_DEMIRROR_ADD: {
        DEMIRROR_SIZE* in;
        UINT32* out;
        status = Input(request, &in);
        if (NT_SUCCESS(status)) {
            status = WdfRequestRetrieveOutputBuffer(request, sizeof(UINT32), (PVOID*)&out, nullptr);
        }
        if (NT_SUCCESS(status)) {
            UINT32 id = 0;
            status = AddMonitor(*in, &id);
            if (NT_SUCCESS(status)) {
                *out = id;
                written = sizeof(UINT32);
            }
        }
        break;
    }
    case IOCTL_DEMIRROR_REMOVE:
    case IOCTL_DEMIRROR_PING: {
        UINT32* id;
        status = Input(request, &id);
        if (NT_SUCCESS(status)) {
            status = code == IOCTL_DEMIRROR_REMOVE ? RemoveMonitor(*id) : PingMonitor(*id);
        }
        break;
    }
    case IOCTL_DEMIRROR_RESIZE: {
        DEMIRROR_RESIZE* in;
        status = Input(request, &in);
        if (NT_SUCCESS(status)) {
            status = ResizeMonitor(*in);
        }
        break;
    }
    default:
        status = STATUS_INVALID_DEVICE_REQUEST;
    }
    WdfRequestCompleteWithInformation(request, status, written);
}

NTSTATUS EvtAdapterInitFinished(IDDCX_ADAPTER, const IDARG_IN_ADAPTER_INIT_FINISHED* in) {
    if (NT_SUCCESS(in->AdapterInitStatus)) {
        std::lock_guard<std::mutex> lock(g_lock);
        g_ready = true;
    }
    return STATUS_SUCCESS;
}

NTSTATUS EvtAdapterCommitModes(IDDCX_ADAPTER, const IDARG_IN_COMMITMODES*) {
    return STATUS_SUCCESS;
}

// The modes of a monitor, from its EDID's serial number: the size it was asked for.
NTSTATUS EvtParseMonitorDescription(const IDARG_IN_PARSEMONITORDESCRIPTION* in, IDARG_OUT_PARSEMONITORDESCRIPTION* out) {
    out->MonitorModeBufferOutputCount = 1;
    if (in->MonitorModeBufferInputCount < 1) {
        // No buffer: the caller only wants the count.
        return in->MonitorModeBufferInputCount > 0 ? STATUS_BUFFER_TOO_SMALL : STATUS_SUCCESS;
    }
    UINT32 width = 1920, height = 1080;
    if (in->MonitorDescription.DataSize >= 128 && in->MonitorDescription.pData) {
        UINT serial;
        memcpy(&serial, (const BYTE*)in->MonitorDescription.pData + 12, 4);
        std::lock_guard<std::mutex> lock(g_lock);
        if (Monitor* m = serial ? ByConnector(serial - 1) : nullptr) {
            width = m->width;
            height = m->height;
        }
    }
    IDDCX_MONITOR_MODE& mode = in->pMonitorModes[0];
    mode = {};
    mode.Size = sizeof(mode);
    mode.Origin = IDDCX_MONITOR_MODE_ORIGIN_DRIVER;
    Signal(mode.MonitorVideoSignalInfo, width, height, true);
    out->PreferredMonitorModeIdx = 0;
    return STATUS_SUCCESS;
}

// Only for monitors without an EDID, which this driver doesn't make.
NTSTATUS EvtMonitorGetDefaultModes(IDDCX_MONITOR, const IDARG_IN_GETDEFAULTDESCRIPTIONMODES*, IDARG_OUT_GETDEFAULTDESCRIPTIONMODES* out) {
    out->DefaultMonitorModeBufferOutputCount = 0;
    out->PreferredMonitorModeIdx = NO_PREFERRED_MODE;
    return STATUS_SUCCESS;
}

NTSTATUS EvtMonitorQueryTargetModes(IDDCX_MONITOR object, const IDARG_IN_QUERYTARGETMODES* in, IDARG_OUT_QUERYTARGETMODES* out) {
    UINT32 width = 1920, height = 1080;
    {
        std::lock_guard<std::mutex> lock(g_lock);
        if (Monitor* m = ByObject(object)) {
            width = m->width;
            height = m->height;
        }
    }
    out->TargetModeBufferOutputCount = 1;
    if (in->TargetModeBufferInputCount >= 1) {
        TargetMode(in->pTargetModes[0], width, height);
    }
    return STATUS_SUCCESS;
}

HRESULT RenderDevice(LUID luid, ComPtr<ID3D11Device>& device) {
    ComPtr<IDXGIFactory4> factory;
    HRESULT hr = CreateDXGIFactory2(0, IID_PPV_ARGS(&factory));
    if (FAILED(hr)) {
        return hr;
    }
    ComPtr<IDXGIAdapter1> adapter;
    hr = factory->EnumAdapterByLuid(luid, IID_PPV_ARGS(&adapter));
    if (FAILED(hr)) {
        return hr;
    }
    return D3D11CreateDevice(adapter.Get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                             nullptr, 0, D3D11_SDK_VERSION, &device, nullptr, nullptr);
}

NTSTATUS EvtMonitorAssignSwapChain(IDDCX_MONITOR object, const IDARG_IN_SETSWAPCHAIN* in) {
    std::unique_ptr<Worker> old;
    {
        std::lock_guard<std::mutex> lock(g_lock);
        if (Monitor* m = ByObject(object)) {
            old = std::move(m->worker);
        }
    }
    old.reset();

    ComPtr<ID3D11Device> device;
    if (FAILED(RenderDevice(in->RenderAdapterLuid, device))) {
        // Deleting the swap chain makes Windows make a new one and try again.
        WdfObjectDelete((WDFOBJECT)in->hSwapChain);
        return STATUS_SUCCESS;
    }
    auto worker = std::make_unique<Worker>(in->hSwapChain, in->hNextSurfaceAvailable, device);
    {
        std::lock_guard<std::mutex> lock(g_lock);
        if (Monitor* m = ByObject(object)) {
            m->worker = std::move(worker);
        }
    }
    // A monitor removed meanwhile: `worker` stops here.
    return STATUS_SUCCESS;
}

NTSTATUS EvtMonitorUnassignSwapChain(IDDCX_MONITOR object) {
    std::unique_ptr<Worker> worker;
    {
        std::lock_guard<std::mutex> lock(g_lock);
        if (Monitor* m = ByObject(object)) {
            worker = std::move(m->worker);
        }
    }
    return STATUS_SUCCESS;
}

NTSTATUS EvtDeviceD0Entry(WDFDEVICE device, WDF_POWER_DEVICE_STATE) {
    // A fresh adapter: monitors from before (if the device restarted) are gone with the old one.
    std::vector<std::unique_ptr<Monitor>> old;
    {
        std::lock_guard<std::mutex> lock(g_lock);
        old.swap(g_monitors);
        g_ready = false;
    }
    old.clear();

    IDDCX_ENDPOINT_VERSION version = {};
    version.Size = sizeof(version);
    version.MajorVer = DEMIRROR_IDD_VERSION;

    IDDCX_ADAPTER_CAPS caps = {};
    caps.Size = sizeof(caps);
    caps.MaxMonitorsSupported = DEMIRROR_MAX_MONITORS;
    caps.EndPointDiagnostics.Size = sizeof(caps.EndPointDiagnostics);
    caps.EndPointDiagnostics.GammaSupport = IDDCX_FEATURE_IMPLEMENTATION_NONE;
    caps.EndPointDiagnostics.TransmissionType = IDDCX_TRANSMISSION_TYPE_WIRED_OTHER;
    caps.EndPointDiagnostics.pEndPointFriendlyName = L"demirror virtual display";
    caps.EndPointDiagnostics.pEndPointManufacturerName = L"demirror";
    caps.EndPointDiagnostics.pEndPointModelName = L"demirror";
    caps.EndPointDiagnostics.pFirmwareVersion = &version;
    caps.EndPointDiagnostics.pHardwareVersion = &version;

    WDF_OBJECT_ATTRIBUTES attributes;
    WDF_OBJECT_ATTRIBUTES_INIT(&attributes);
    IDARG_IN_ADAPTER_INIT init = {};
    init.WdfDevice = device;
    init.pCaps = &caps;
    init.ObjectAttributes = &attributes;
    IDARG_OUT_ADAPTER_INIT out = {};
    NTSTATUS status = IddCxAdapterInitAsync(&init, &out);
    if (NT_SUCCESS(status)) {
        std::lock_guard<std::mutex> lock(g_lock);
        g_adapter = out.AdapterObject;
    }
    return status;
}

NTSTATUS EvtDeviceAdd(WDFDRIVER, PWDFDEVICE_INIT init) {
    WDF_PNPPOWER_EVENT_CALLBACKS power;
    WDF_PNPPOWER_EVENT_CALLBACKS_INIT(&power);
    power.EvtDeviceD0Entry = EvtDeviceD0Entry;
    WdfDeviceInitSetPnpPowerEventCallbacks(init, &power);

    IDD_CX_CLIENT_CONFIG config;
    IDD_CX_CLIENT_CONFIG_INIT(&config);
    config.EvtIddCxDeviceIoControl = EvtIoControl;
    config.EvtIddCxAdapterInitFinished = EvtAdapterInitFinished;
    config.EvtIddCxAdapterCommitModes = EvtAdapterCommitModes;
    config.EvtIddCxParseMonitorDescription = EvtParseMonitorDescription;
    config.EvtIddCxMonitorGetDefaultDescriptionModes = EvtMonitorGetDefaultModes;
    config.EvtIddCxMonitorQueryTargetModes = EvtMonitorQueryTargetModes;
    config.EvtIddCxMonitorAssignSwapChain = EvtMonitorAssignSwapChain;
    config.EvtIddCxMonitorUnassignSwapChain = EvtMonitorUnassignSwapChain;
    NTSTATUS status = IddCxDeviceInitConfig(init, &config);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    WDF_OBJECT_ATTRIBUTES attributes;
    WDF_OBJECT_ATTRIBUTES_INIT(&attributes);
    WDFDEVICE device = nullptr;
    status = WdfDeviceCreate(&init, &attributes, &device);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    // Where demirror finds the driver (protocol.h).
    status = WdfDeviceCreateDeviceInterface(device, &DEMIRROR_IDD_INTERFACE, nullptr);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    WDF_TIMER_CONFIG timer;
    WDF_TIMER_CONFIG_INIT_PERIODIC(&timer, EvtWatchdog, 1000);
    WDF_OBJECT_ATTRIBUTES timerAttributes;
    WDF_OBJECT_ATTRIBUTES_INIT(&timerAttributes);
    timerAttributes.ParentObject = device;
    status = WdfTimerCreate(&timer, &timerAttributes, &g_watchdog);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    WdfTimerStart(g_watchdog, WDF_REL_TIMEOUT_IN_MS(1000));

    return IddCxDeviceInitialize(device);
}

} // namespace

extern "C" DRIVER_INITIALIZE DriverEntry;

extern "C" NTSTATUS DriverEntry(PDRIVER_OBJECT driver, PUNICODE_STRING registryPath) {
    WDF_DRIVER_CONFIG config;
    WDF_DRIVER_CONFIG_INIT(&config, EvtDeviceAdd);
    return WdfDriverCreate(driver, registryPath, WDF_NO_OBJECT_ATTRIBUTES, &config, WDF_NO_HANDLE);
}
