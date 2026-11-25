import { invoke, Channel } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

// --- typy zgodne z backendem (serde camelCase) --- //

export interface ToolsInfo {
  adbPath: string | null;
  fastbootPath: string | null;
  adbVersion: string | null;
}

export type ConnMode =
  | 'adb'
  | 'recovery'
  | 'sideload'
  | 'fastboot'
  | 'mtp'
  | 'unauthorized'
  | 'offline'
  | 'nopermissions';

export interface Device {
  serial: string;
  mode: ConnMode;
  modelHint: string | null;
  product: string | null;
  transportId: string | null;
  wireless: boolean;
}

export interface DeviceInfo {
  serial: string;
  source: 'adb' | 'fastboot' | 'mtp';
  manufacturer?: string | null;
  brand?: string | null;
  model?: string | null;
  device?: string | null;
  androidVersion?: string | null;
  sdk?: string | null;
  buildId?: string | null;
  fingerprint?: string | null;
  securityPatch?: string | null;
  baseband?: string | null;
  bootloader?: string | null;
  soc?: string | null;
  hardware?: string | null;
  cpuAbi?: string | null;
  ramKb?: number | null;
  storageTotalKb?: number | null;
  storageUsedKb?: number | null;
  resolution?: string | null;
  kernel?: string | null;
  batteryLevel?: number | null;
  batteryStatus?: string | null;
  batteryHealth?: string | null;
  batteryTempC?: number | null;
  batteryVoltageMv?: number | null;
  serialNo?: string | null;
  imei?: string | null;
  csc?: string | null;
  verifiedBoot?: string | null;
  bootloaderLocked?: boolean | null;
  slot?: string | null;
  extra: [string, string][];
}

export interface DirListing {
  path: string;
  dirs: string[];
  files: string[];
}

export interface Partition {
  mount: string;
  size: string;
  used: string;
  avail: string;
  readable: boolean;
}

export type LogLevel = 'info' | 'ok' | 'warn' | 'error';

export interface FailedItem {
  path: string;
  error: string;
}

export interface ProgressData {
  done: number;
  total: number;
  bytesDone: number;
  bytesTotal: number;
  current: string | null;
}

export interface FinishedData {
  ok: number;
  skipped: number;
  total: number;
  failures: FailedItem[];
  cancelled: boolean;
}

// Zdarzenia zadania (serde tag = "kind")
export type TransferEvent =
  | { kind: 'log'; level: LogLevel; message: string }
  | ({ kind: 'progress' } & ProgressData)
  | { kind: 'item'; id: string; ok: boolean; message: string }
  | ({ kind: 'finished' } & FinishedData)
  | { kind: 'fatal'; message: string };

export interface SelectedItem {
  path: string;
  isFile: boolean;
}

export interface PullOptions {
  items: SelectedItem[];
  localRoot: string;
  workers?: number;
  timeoutSeconds?: number;
  resume?: boolean;
  skipExisting?: boolean;
  preserveTime?: boolean;
  /** nazwy zgodne z NTFS/FAT32/exFAT (`: * ? " < > |` -> `_`) */
  ntfsSafe?: boolean;
  /** użyj roota (su / adbd root), jeśli dostępny */
  useRoot?: boolean;
}

export interface BackupOptions {
  destDir: string;
  waitMinutes?: number;
}

export interface BackupResult {
  dir: string;
  contacts: number;
  sms: number;
  contactsDenied: boolean;
  smsDenied: boolean;
  files: string[];
  device: string;
}

export interface RestoreOptions {
  sourceDir: string;
  waitMinutes?: number;
}

export interface RestoreFileInfo {
  path: string;
  size: number;
}

export interface RestoreDirInfo {
  contacts: RestoreFileInfo | null;
  sms: RestoreFileInfo | null;
}

export interface RestoreResult {
  contactsFile: boolean;
  contactsCards: number;
  /** 'intent' = aplikacja Kontakty, 'direct' = import agenta, 'none' */
  contactsMethod: 'intent' | 'direct' | 'none';
  contactsImported: number;
  smsFile: boolean;
  smsTotal: number;
  smsInserted: number;
  smsSkipped: number;
  smsFailed: number;
  mmsSkipped: number;
  smsRestored: boolean;
  defaultSmsChanged: boolean;
  defaultSmsRestored: boolean;
  previousSmsApp: string;
  incomingSmsSaved: number;
  incomingSmsLost: number;
  incomingMms: number;
  manualInstall: boolean;
  device: string;
}

export type RootMode = 'none' | 'adbd' | 'su' | 'suZero';

export interface RootInfo {
  mode: RootMode;
  suPresent: boolean;
  detail: string;
}

export interface PushOptions {
  items: string[];
  remoteDest: string;
  workers?: number;
  timeoutSeconds?: number;
  skipExisting?: boolean;
}

export interface AppEntry {
  package: string;
  apkPath: string | null;
  system: boolean;
  enabled: boolean;
  installed: boolean;
  critical: boolean;
}

export interface AppDetails {
  package: string;
  label: string | null;
  versionName: string | null;
  icon: string | null;
  iconBg: string | null;
  iconBgImage: string | null;
  adaptive: boolean;
  apkSize: number;
}

export type AppAction = 'uninstall' | 'uninstallFull' | 'disable' | 'enable' | 'restore' | 'forceStop' | 'clear';

/** Minimalne SDK dla akcji (zgodne z backendem `AppAction::min_sdk`). */
export const ACTION_MIN_SDK: Record<AppAction, number> = {
  uninstall: 21,
  uninstallFull: 1,
  disable: 14,
  enable: 1,
  restore: 21,
  forceStop: 1,
  clear: 1
};

// Strumień ładowania listy aplikacji (serde tag = "kind")
export type AppsLoadEvent =
  | { kind: 'stage'; stage: 'list' | 'details' }
  | { kind: 'list'; apps: AppEntry[]; sdk: number }
  | { kind: 'details'; details: AppDetails }
  | { kind: 'progress'; done: number; total: number; current: string | null }
  | { kind: 'finished'; cancelled: boolean; failed: number }
  | { kind: 'error'; message: string };

export interface DistroHint {
  id: string;
  name: string;
  command: string;
  note: string | null;
}

export interface ScrcpyStatus {
  path: string | null;
  version: string | null;
  bundled: boolean;
  os: string;
  distro: DistroHint | null;
  canDownload: boolean;
}

export interface ScrcpyOptions {
  maxSize: number;
  bitRateMbps: number;
  maxFps: number;
  stayAwake: boolean;
  showTouches: boolean;
  turnScreenOff: boolean;
  noAudio: boolean;
  alwaysOnTop: boolean;
  borderless: boolean;
  fullscreen: boolean;
  viewOnly: boolean;
}

export interface Screenshot {
  path: string;
  dataUrl: string;
  width: number;
  height: number;
  bytes: number;
  takenAt: string;
}

export interface InstallOptions {
  downgrade?: boolean;
  grantAll?: boolean;
}

export interface ShellOutput {
  status: number;
  stdout: string;
  stderr: string;
}

export type RebootMode =
  | 'system'
  | 'recovery'
  | 'download'
  | 'bootloader'
  | 'fastbootd'
  | 'sideload'
  | 'edl'
  | 'poweroff';


// ---------------------------------------------------------------- v0.4 ---- //

export type UsbKind =
  | 'adb' | 'fastboot' | 'mtp' | 'samsungDownload' | 'qualcommEdl' | 'qualcommDiag'
  | 'mediatekBrom' | 'mediatekPreloader' | 'android';

export interface UsbDevice {
  vid: string;
  pid: string;
  vendorName: string | null;
  manufacturer: string | null;
  product: string | null;
  serial: string | null;
  kind: UsbKind;
  drivers: string[];
  hasDriver: boolean;
  accessible: boolean | null;
  interfaces: string[];
}

export interface DriverStatus {
  id: string;
  name: string;
  description: string;
  kind: 'builtin' | 'inf' | 'installer';
  installed: boolean | null;
  foundIn: string[];
  url: string | null;
  args: string;
  sha256: string | null;
  officialPage: string | null;
  devicesMissingDriver: number;
}

export interface UdevStatus {
  rulesFiles: string[];
  covered: string[];
  missing: string[];
  pkexec: boolean;
  sudo: boolean;
  user: string;
  inAdbusers: boolean;
  packageHint: string | null;
}

export interface DriversReport {
  os: string;
  drivers: DriverStatus[];
  devices: UsbDevice[];
  udev: UdevStatus | null;
  problems: string[];
}

export type UdevInstallResult =
  | { result: 'installed'; source: string; path: string }
  | { result: 'needPassword'; reason: string }
  | { result: 'wrongPassword' }
  | { result: 'cancelled' };

export interface StatusRow {
  key: string;
  label: string;
  value: string;
  level: 'ok' | 'warn' | 'bad' | 'info';
}

export interface SamsungStatus {
  isSamsung: boolean;
  model: string | null;
  knox: string | null;
  knoxGuard: string | null;
  binary: string | null;
  rows: StatusRow[];
}

export interface DownloadModeStatus {
  present: boolean;
  devices: UsbDevice[];
  driverOk: boolean | null;
}

export interface ArbInfo {
  anti: number | null;
  supported: boolean;
  raw: string;
  product: string | null;
  unlocked: boolean | null;
  slot: string | null;
}

export interface FirmwareArb {
  folder: string;
  anti: number | null;
  script: string | null;
  product: string | null;
}

export interface EdlStatus {
  present: boolean;
  devices: UsbDevice[];
  ready: boolean;
  detail: string;
  otherModes: UsbDevice[];
}

export interface MotoStep {
  index: number;
  op: 'flash' | 'erase' | 'getvar' | 'oem' | 'rebootBootloader' | 'reboot' | 'unknown';
  operation: string;
  partition: string | null;
  filename: string | null;
  var: string | null;
  md5: string | null;
  size: number | null;
  fileExists: boolean;
  command: string;
  wipesData: boolean;
}

export interface MotoFirmware {
  dir: string;
  xmlFile: string;
  phoneModel: string | null;
  softwareVersion: string | null;
  steps: MotoStep[];
  missingFiles: string[];
  totalBytes: number;
  wipesData: boolean;
  alternatives: string[];
}

export interface MotoFlashOptions {
  dir: string;
  xmlFile: string | null;
  keepData: boolean;
  verifyMd5: boolean;
  rebootAfter: boolean;
}

export interface BatteryInfo {
  source: string;
  capacity: number | null;
  cycleCount: number | null;
  voltageMv: number | null;
  currentMa: number | null;
  tempC: number | null;
  status: string | null;
  health: string | null;
  chargeFullMah: number | null;
  chargeFullDesignMah: number | null;
  healthPct: number | null;
  technology: string | null;
  chargeType: string | null;
  powerW: number | null;
}

export interface DiagSnapshot {
  ts: number;
  battery: BatteryInfo;
  thermal: { name: string; tempC: number }[];
  sources: { name: string; kind: string; online: boolean }[];
  unsupported: string[];
}

export type DiagEvent =
  | { kind: 'snapshot'; data: DiagSnapshot }
  | { kind: 'error'; message: string }
  | { kind: 'stopped'; reason: string };

export interface LogLine {
  id: number;
  ts: string;
  pid: string;
  level: string;
  tag: string;
  msg: string;
}

export type LogcatEvent =
  | { kind: 'lines'; lines: LogLine[]; dropped: number }
  | { kind: 'reset'; lines: LogLine[] }
  | { kind: 'stats'; total: number; matched: number; perSec: number; buffered: number }
  | { kind: 'ended'; message: string };

function rawChannel<T>(cb: (e: T) => void): Channel<T> {
  const ch = new Channel<T>();
  ch.onmessage = cb;
  return ch;
}


// ---------------------------------------------------------------- v0.5 ---- //

export interface DownloadProbe {
  present: boolean;
  devices: UsbDevice[];
  accessible: boolean;
  /** flashowanie w toku - sonda USB wstrzymana */
  busy: boolean;
}

export interface PitEntry {
  partitionName: string;
  flashFilename: string;
  partitionId: number;
  blockCount: number;
  binaryType: number;
  deviceType: number;
}

export interface HeimdallStatus {
  path: string | null;
  version: string | null;
  os: string;
  installHint: string | null;
}

export type TarRole = 'bl' | 'ap' | 'cp' | 'csc' | 'homeCsc' | 'userdata' | 'unknown';

export interface TarImage { name: string; size: number; offset: number; }

export interface FirmwareTar {
  path: string;
  filename: string;
  role: TarRole;
  images: TarImage[];
  totalSize: number;
  fileSize: number;
  hasMd5: boolean;
  md5Expected: string | null;
}

export interface FirmwareSet {
  parts: FirmwareTar[];
  totalImages: number;
  totalSize: number;
  hasUserdata: boolean;
}

export interface SamsungFlashOptions {
  parts: string[];
  verifyMd5: boolean;
  autoReboot: boolean;
  repartition: boolean;
  pitPath: string | null;
  acknowledged: boolean;
}

export type AdbActionReq =
  | { kind: 'secretCode'; code: string }
  | { kind: 'openUrl'; url: string }
  | { kind: 'skipSetupWizard' }
  | { kind: 'localeSettings' }
  | { kind: 'setLocaleRoot'; locale: string }
  | { kind: 'startAction'; action: string; data: string | null }
  | { kind: 'startActivity'; component: string }
  | { kind: 'amRaw'; args: string };


// --- APK Inspector & DevTools --- //

export interface ToolchainStatus {
  java: string | null;
  javaVersion: string | null;
  javaBundled: boolean;
  apktool: string | null;
  signer: string | null;
  keystore: boolean;
  toolsDir: string;
  canDownloadJava: boolean;
  ready: boolean;
}

export interface PatchOptions {
  debuggable: boolean;
  newPackage: string | null;
  newLabel: string | null;
}

export interface BuildOptions {
  patch: PatchOptions;
  install: boolean;
  serial: string | null;
  uninstallFirst: boolean;
  exportDir: string | null;
}

export type OpenSource =
  | { kind: 'device'; serial: string; package: string }
  | { kind: 'file'; path: string; serial: string | null };

export interface BuildInfo { at: number; package: string; files: string[]; changes: string[]; }

export interface WorkspaceMeta {
  id: string;
  package: string;
  label: string | null;
  versionName: string | null;
  sourceKind: 'device' | 'file';
  source: string;
  created: number;
  apks: string[];
  size: number;
  decoded: boolean;
  patch: PatchOptions;
  replaced: string[];
  lastBuild: BuildInfo | null;
}

export interface ManifestInfo {
  package: string;
  debuggable: boolean;
  label: string | null;
  applicationClass: string | null;
  activities: number;
  services: number;
  receivers: number;
  providers: string[];
  declaredPermissions: string[];
  usesPermissions: number;
  hasSplitAttr: boolean;
  versionName: string | null;
  versionCode: string | null;
  minSdk: string | null;
  targetSdk: string | null;
  renamePackage: string | null;
}

export interface DevWorkspace { meta: WorkspaceMeta; manifest: ManifestInfo; }

export interface PatchResult { manifest: string; changes: string[]; }

export interface BuildResult {
  workspace: string;
  package: string;
  files: string[];
  exported: string[];
  changes: string[];
  installed: boolean;
}

export interface ResEntry {
  name: string;
  rel: string;
  dir: boolean;
  size: number;
  kind: 'dir' | 'image' | 'xml' | 'text' | 'font' | 'audio' | 'binary';
  replaced: boolean;
  children: number;
}

export interface ResPreview {
  rel: string;
  kind: string;
  size: number;
  mime: string | null;
  data: string | null;
  text: string | null;
  truncated: boolean;
  ninePatch: boolean;
  replaced: boolean;
  replaceable: boolean;
}

// --- ADB przez Wi-Fi --- //

export interface WifiResult {
  serial: string;
  address: string;
  state: string;
  via: 'manual' | 'mdns-auto' | 'mdns' | 'tcpip';
}

export type WifiStageKey = 'pair' | 'discover' | 'connect' | 'ip' | 'tcpip' | 'unplug';

export interface WifiStage {
  stage: WifiStageKey;
  detail: string;
}

export interface PairRequest {
  host: string;
  pairPort: number;
  code: string;
  connectPort: number | null;
}

export type InstallKind =
  | 'windowsInstaller'
  | 'windowsPortable'
  | 'linuxAppImage'
  | 'linuxPortable'
  | 'linuxSystem'
  | 'dev'
  | 'unsupported';

export interface UpdateAsset {
  url: string;
  filename: string;
  size: number;
  sha256: string;
}

export interface UpdateInfo {
  current: string;
  latest: string | null;
  available: boolean;
  notesPl: string;
  notesEn: string;
  publishedAt: string | null;
  kind: InstallKind;
  asset: UpdateAsset | null;
  /** 'manifest' (latest.json) | 'api' (tylko informacja) */
  source: 'manifest' | 'api';
  releaseUrl: string;
  canInstall: boolean;
}

export type UpdateOutcome = { action: 'restart' } | { action: 'manual'; path: string; command: string };

// --- Smart EI (Smart Export Import) --- //

export type SeiRole = 'export' | 'import';
export type SeiConnection = 'adb' | 'wifi';

export interface SeiModules {
  /** dcim | pictures | movies | music | download | documents | whatsapp */
  files: string[];
  contacts: boolean;
  sms: boolean;
  wallpaper: boolean;
  settings: boolean;
  apps: boolean;
  apks: boolean;
  homescreen: boolean;
}

export interface SeiStartOptions {
  role: SeiRole;
  connection: SeiConnection;
  serial?: string | null;
  dir: string;
  modules: SeiModules;
  bindIp?: string | null;
}

export interface SeiSessionInfo {
  sessionId: string;
  jobId: string;
  role: SeiRole;
  connection: SeiConnection;
  dir: string;
  ip: string;
  port: number;
  pin: string;
  pairUrl: string;
  pairQr: string;
  apkUrl: string;
  apkQr: string;
  agentApk: string;
}

export interface SeiNetIf {
  name: string;
  ip: string;
  virtualIf: boolean;
  recommended: boolean;
}

export interface SeiDevice {
  manufacturer: string;
  brand: string;
  model: string;
  device: string;
  sdk: number;
  androidVersion: string;
}

export interface SeiModuleEntry {
  name: string;
  ok: boolean;
  items: number;
  bytes: number;
  note: string;
}

export interface SeiManifest {
  format: string;
  formatVersion: number;
  tool: string;
  created: string;
  completed: string | null;
  complete: boolean;
  connection: SeiConnection;
  source: SeiDevice;
  modules: SeiModuleEntry[];
  notes: string[];
}

export interface SeiImportApp {
  package: string;
  label: string;
  source: 'play_store' | 'sideload';
  versionName: string;
  size: number;
  hasApk: boolean;
  selected: boolean;
  reason: 'sourceVendor' | 'alreadyInstalled' | 'noApk' | null;
  vendor: string | null;
}

export interface SeiQueueItem {
  package: string;
  label: string;
  status: 'pending' | 'opened' | 'installed' | 'advanced' | 'skipped';
}

export interface SeiQueue {
  active: boolean;
  finished?: boolean;
  position?: number;
  total?: number;
  rev?: number;
  current?: SeiQueueItem | null;
  items?: SeiQueueItem[];
}

export interface SeiSelection {
  modules: SeiModules;
  packages: string[];
}

type OnEvent = (e: TransferEvent) => void;

function channel(onEvent: OnEvent): Channel<TransferEvent> {
  const ch = new Channel<TransferEvent>();
  ch.onmessage = onEvent;
  return ch;
}

// --- wrappery komend --- //

export const api = {
  updateCheck: () => invoke<UpdateInfo>('update_check'),
  updateInstall: (onEvent: OnEvent) => invoke<string>('update_install', { onEvent: channel(onEvent) }),
  updateInstallPackage: (path: string) => invoke<string>('update_install_package', { path }),
  locateTools: () => invoke<ToolsInfo>('locate_tools'),
  setAdbPath: (path: string) => invoke<ToolsInfo>('set_adb_path', { path }),
  scanDevices: () => invoke<Device[]>('scan_devices'),
  deviceInfo: (serial: string, mode: ConnMode) => invoke<DeviceInfo>('device_info', { serial, mode }),
  listDir: (serial: string, path: string, root = false) => invoke<DirListing>('list_dir', { serial, path, root }),
  detectRoot: (serial: string, force = false) => invoke<RootInfo>('detect_root', { serial, force }),
  scanPartitions: (serial: string) => invoke<Partition[]>('scan_partitions', { serial }),
  reboot: (serial: string, mode: RebootMode, via: 'adb' | 'fastboot') =>
    invoke<void>('reboot', { serial, mode, via }),

  startPull: (serial: string, options: PullOptions, onEvent: OnEvent) =>
    invoke<string>('start_pull', { serial, options, onEvent: channel(onEvent) }),
  startContactsBackup: (serial: string, options: BackupOptions, onEvent: OnEvent) =>
    invoke<string>('start_contacts_backup', { serial, options, onEvent: channel(onEvent) }),
  startContactsRestore: (serial: string, options: RestoreOptions, onEvent: OnEvent) =>
    invoke<string>('start_contacts_restore', { serial, options, onEvent: channel(onEvent) }),
  inspectRestoreDir: (dir: string) => invoke<RestoreDirInfo>('inspect_restore_dir', { dir }),
  /** Przycisk "Gotowe" w oknie ręcznej instalacji agenta (plan B). */
  agentInstallConfirm: (serial: string) => invoke<boolean>('agent_install_confirm', { serial }),

  smarteiInterfaces: () => invoke<SeiNetIf[]>('smartei_interfaces'),
  smarteiInspect: (dir: string) => invoke<{ manifest: SeiManifest; apps: number }>('smartei_inspect', { dir }),
  /** Kopia Agent.apk w folderze danych programu (do pokazania w menedżerze plików). */
  smarteiAgentApk: () => invoke<string>('smartei_agent_apk'),
  smarteiStart: (options: SeiStartOptions, onEvent: OnEvent) =>
    invoke<SeiSessionInfo>('smartei_start', { options, onEvent: channel(onEvent) }),
  smarteiProceed: (sessionId: string, selection: SeiSelection) =>
    invoke<boolean>('smartei_proceed', { sessionId, selection }),
  smarteiQueue: (sessionId: string, action: 'next' | 'skip') => invoke<boolean>('smartei_queue', { sessionId, action }),
  startPush: (serial: string, options: PushOptions, onEvent: OnEvent) =>
    invoke<string>('start_push', { serial, options, onEvent: channel(onEvent) }),
  cancelJob: (jobId: string) => invoke<void>('cancel_job', { jobId }),

  listApps: (serial: string) => invoke<AppEntry[]>('list_apps', { serial }),
  loadApps: (serial: string, lang: string, onEvent: (e: AppsLoadEvent) => void) => {
    const ch = new Channel<AppsLoadEvent>();
    ch.onmessage = onEvent;
    return invoke<string>('load_apps', { serial, lang, onEvent: ch });
  },
  appsAction: (serial: string, packages: string[], action: AppAction, onEvent: OnEvent) =>
    invoke<string>('apps_action', { serial, packages, action, onEvent: channel(onEvent) }),
  extractApks: (serial: string, packages: string[], outDir: string, onEvent: OnEvent) =>
    invoke<string>('extract_apks', { serial, packages, outDir, onEvent: channel(onEvent) }),
  installApks: (serial: string, paths: string[], options: InstallOptions, onEvent: OnEvent) =>
    invoke<string>('install_apks', { serial, paths, options, onEvent: channel(onEvent) }),

  shellCommand: (serial: string, command: string) => invoke<ShellOutput>('shell_command', { serial, command }),
  startSideload: (serial: string, path: string, onEvent: OnEvent) =>
    invoke<string>('start_sideload', { serial, path, onEvent: channel(onEvent) }),

  scrcpyStatus: () => invoke<ScrcpyStatus>('scrcpy_status'),
  scrcpyDownload: (onEvent: OnEvent) => invoke<string>('scrcpy_download', { onEvent: channel(onEvent) }),
  scrcpyStart: (serial: string, title: string, options: ScrcpyOptions, onEvent: OnEvent) =>
    invoke<string>('scrcpy_start', { serial, title, options, onEvent: channel(onEvent) }),
  takeScreenshot: (serial: string) => invoke<Screenshot>('take_screenshot', { serial }),
  revealPath: (path: string) => invoke<void>('reveal_path', { path }),

  driversReport: () => invoke<DriversReport>('drivers_report'),
  driversSetOverride: (id: string, url: string | null, args: string | null, sha256: string | null) =>
    invoke<void>('drivers_set_override', { id, url, args, sha256 }),
  driverInstall: (id: string, localPath: string | null, onEvent: OnEvent) =>
    invoke<string>('driver_install', { request: { id, localPath }, onEvent: channel(onEvent) }),
  udevInstall: (password: string | null, onEvent: OnEvent) =>
    invoke<UdevInstallResult>('udev_install', { password, onEvent: channel(onEvent) }),
  usbScan: () => invoke<UsbDevice[]>('usb_scan'),

  samsungStatus: (serial: string) => invoke<SamsungStatus>('samsung_status', { serial }),
  samsungDownloadMode: () => invoke<DownloadModeStatus>('samsung_download_mode'),
  xiaomiArb: (serial: string) => invoke<ArbInfo>('xiaomi_arb', { serial }),
  xiaomiFirmwareArb: (folder: string) => invoke<FirmwareArb>('xiaomi_firmware_arb', { folder }),
  edlStatus: () => invoke<EdlStatus>('edl_status'),
  motoLoad: (dir: string, xmlFile: string | null) => invoke<MotoFirmware>('moto_load', { dir, xmlFile }),
  motoFlash: (serial: string, options: MotoFlashOptions, onEvent: OnEvent) =>
    invoke<string>('moto_flash', { serial, options, onEvent: channel(onEvent) }),

  diagSnapshot: (serial: string) => invoke<DiagSnapshot>('diag_snapshot', { serial }),
  diagStart: (serial: string, intervalMs: number, onEvent: (e: DiagEvent) => void) =>
    invoke<string>('diag_start', { serial, intervalMs, onEvent: rawChannel(onEvent) }),

  logcatStart: (serial: string, options: { buffers: string[]; tail: number }, onEvent: (e: LogcatEvent) => void) =>
    invoke<string>('logcat_start', { serial, options, onEvent: rawChannel(onEvent) }),
  logcatFilter: (jobId: string, pattern: string, minLevel: string, caseInsensitive: boolean) =>
    invoke<void>('logcat_filter', { jobId, pattern, minLevel, caseInsensitive }),
  logcatExport: (jobId: string, path: string) => invoke<number>('logcat_export', { jobId, path }),
  logcatClear: (serial: string) => invoke<void>('logcat_clear', { serial }),

  downloadProbe: () => invoke<DownloadProbe>('download_probe'),
  readPit: (savePath: string | null, onEvent: OnEvent) =>
    invoke<string>('read_pit', { savePath, onEvent: channel(onEvent) }),
  heimdallStatus: () => invoke<HeimdallStatus>('heimdall_status'),
  firmwareInspect: (paths: string[]) => invoke<FirmwareSet>('firmware_inspect', { paths }),
  samsungFlash: (options: SamsungFlashOptions, onEvent: OnEvent) =>
    invoke<string>('samsung_flash', { options, onEvent: channel(onEvent) }),

  enterpriseAdb: (serial: string, action: AdbActionReq) => invoke<ShellOutput>('enterprise_adb', { serial, action }),
  enterpriseFastboot: (serial: string, id: string) => invoke<ShellOutput>('enterprise_fastboot', { serial, id }),
  enterpriseMtpReset: () => invoke<string>('enterprise_mtp_reset'),

  wifiPair: (request: PairRequest, onStage: (s: WifiStage) => void) =>
    invoke<WifiResult>('wifi_pair', { request, onStage: rawChannel(onStage) }),
  wifiConnect: (host: string, port: number) => invoke<WifiResult>('wifi_connect', { host, port }),
  wifiTcpip: (serial: string, port: number | null, onStage: (s: WifiStage) => void) =>
    invoke<WifiResult>('wifi_tcpip', { serial, port, onStage: rawChannel(onStage) }),
  wifiDisconnect: (serial: string) => invoke<string>('wifi_disconnect', { serial }),

  devtoolsStatus: () => invoke<ToolchainStatus>('devtools_status'),
  devtoolsSetup: (onEvent: OnEvent) => invoke<string>('devtools_setup', { onEvent: channel(onEvent) }),
  devtoolsOpen: (source: OpenSource, onEvent: OnEvent) =>
    invoke<string>('devtools_open', { source, onEvent: channel(onEvent) }),
  devtoolsBuild: (id: string, options: BuildOptions, onEvent: OnEvent) =>
    invoke<string>('devtools_build', { id, options, onEvent: channel(onEvent) }),
  devtoolsAuto: (serial: string, pkg: string, options: BuildOptions, onEvent: OnEvent) =>
    invoke<string>('devtools_auto', { serial, package: pkg, options, onEvent: channel(onEvent) }),
  devtoolsWorkspaces: () => invoke<WorkspaceMeta[]>('devtools_workspaces'),
  devtoolsWorkspace: (id: string) => invoke<DevWorkspace>('devtools_workspace', { id }),
  devtoolsDelete: (id: string) => invoke<void>('devtools_delete', { id }),
  devtoolsPreviewPatch: (id: string, options: PatchOptions) =>
    invoke<PatchResult>('devtools_preview_patch', { id, options }),
  devtoolsManifest: (id: string) => invoke<string>('devtools_manifest', { id }),
  devtoolsResList: (id: string, rel: string) => invoke<ResEntry[]>('devtools_res_list', { id, rel }),
  devtoolsResPreview: (id: string, rel: string) => invoke<ResPreview>('devtools_res_preview', { id, rel }),
  devtoolsResReplace: (id: string, rel: string, file: string) =>
    invoke<string[]>('devtools_res_replace', { id, rel, file }),
  devtoolsResRestore: (id: string, rel: string) => invoke<void>('devtools_res_restore', { id, rel }),
  devtoolsResExport: (id: string, rel: string, dest: string) => invoke<void>('devtools_res_export', { id, rel, dest }),
  devtoolsImportFramework: (path: string) => invoke<string>('devtools_import_framework', { path }),

  onDevicesChanged: (cb: (d: Device[]) => void): Promise<UnlistenFn> =>
    listen<Device[]>('devices-changed', (e) => cb(e.payload))
};
