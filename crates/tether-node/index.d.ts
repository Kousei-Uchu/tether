export interface PeerSummary {
  name: string;
  platform: string;
  fingerprint: string;
  /** On a trust list: what the device may do here. On a connection: what the host allows this device to do. */
  permissions: string[];
}

/** A device seen on the local network. Nothing here is authenticated. */
export interface NearbyDevice {
  name: string;
  platform: string;
  fingerprint: string;
  /** `host:port`, IPv4 first. */
  addresses: string[];
  /** The advertised key is one this device has paired with. */
  paired: boolean;
  /** False if it speaks a different protocol version. */
  compatible: boolean;
}

export interface FileEntry {
  name: string;
  /** Bytes; 0 for a folder. */
  size: number;
  dir: boolean;
}

export interface TransferStatus {
  active: boolean;
  upload: boolean;
  name: string;
  done: number;
  total: number;
}

export interface EmojiSymbol {
  symbol: string;
  name: string;
}

/**
 * A question for the person. `connection`: allow this paired device to connect?
 * `verify`: do these symbols match what the other device shows?
 */
export interface Prompt {
  id: number;
  kind: 'connection' | 'verify';
  peerName: string;
  peerPlatform: string;
  peerFingerprint: string;
  emoji: EmojiSymbol[];
  /** The same string as digits, `0000-0000-00000`. */
  digits: string;
}

export interface PairingOptions {
  /** Permissions for the device that pairs with the code. Defaults to ["view"]. */
  grant?: string[];
  /** Seconds the code stays valid. Defaults to 300. */
  ttlSecs?: number;
}

export interface HostEvent {
  kind: 'paired' | 'session-opened' | 'session-closed' | 'refused';
  message: string;
}

/** `move`: x,y are 0..1 of the display. `delta`: dx,dy in pixels. `scroll`: lines, positive is down or right. */
export type InputEvent =
  | { kind: 'move'; x: number; y: number; display?: number }
  | { kind: 'delta'; dx: number; dy: number }
  | { kind: 'button'; button: 'left' | 'right' | 'middle' | 'back' | 'forward'; down: boolean }
  | { kind: 'scroll'; dx?: number; dy?: number }
  | { kind: 'key'; code: string; down: boolean }
  | { kind: 'text'; text: string };

export interface ParsedLink {
  address: string;
  code: string;
  name?: string;
}

/** The link a host shows as a QR code: tether://pair?addr=...&code=...&name=... */
export function pairingLink(address: string, code: string, name?: string | null): string;
/** Reads a pairing link. Throws a readable error if it is not one. */
export function parsePairingLink(link: string): ParsedLink;

export type PermissionName = 'view' | 'input' | 'media' | 'power' | 'files';

/** This machine's identity and trust store. */
export class Device {
  /** `home` overrides the data directory. */
  constructor(home?: string | null, name?: string | null);
  readonly name: string;
  readonly fingerprint: string;
  readonly home: string;
  /** Where Rust panics are written; look here first after a crash. */
  readonly crashLog: string;
  peers(): PeerSummary[];
  grant(query: string, permissions: string[]): PeerSummary[];
  revoke(query: string, permissions: string[]): PeerSummary[];
  /** Returns how many devices were unpaired. */
  remove(query: string): number;
  /** Questions waiting for the person. Poll this and call answerPrompt. */
  pendingPrompts(): Prompt[];
  /** False if the question already timed out. */
  answerPrompt(id: number, accept: boolean): boolean;
  /** With `verify: true`, pairing also asks the person to compare symbols with the host. Off by default. */
  pair(address: string, code: string, verify?: boolean | null): Promise<PeerSummary>;
  connect(address: string): Promise<Connection>;
  /** Looks for Tether devices on the local network for `seconds` (default 3, at most 15). */
  discover(seconds?: number | null): Promise<NearbyDevice[]>;
  /** Finds a paired host by fingerprint and connects, for when its saved address stopped working. */
  quickConnect(fingerprint: string): Promise<Connection>;
  /** `video` lets permitted devices watch a stream: "test-pattern", "screen" or "screen:<id>" (see listDisplays). */
  /** `input: true` lets devices with the "input" permission control this computer. Off by default. */
  /** `clipboard: true` lets devices with the "files" permission read and replace this computer's clipboard text. Off by default. */
  listen(
    bind: string,
    pairing?: PairingOptions | null,
    video?: string | null,
    input?: boolean | null,
    clipboard?: boolean | null,
    /** Adds sound to the video: "system" (what the computer plays) or "tone". Needs video. */
    audio?: string | null,
    /** Makes this a wake relay: each entry is "name=MAC" or "name=MAC@broadcast". */
    wake?: string[] | null,
    /** Announce this device on the local network. Defaults to true. */
    advertise?: boolean | null,
    /** Ask before each connection from a paired device (see pendingPrompts). Off by default. */
    confirm?: boolean | null,
    /** Ask the person to compare symbols when someone pairs (see pendingPrompts). Off by default. */
    verify?: boolean | null,
    /** A folder to share with devices that hold `files`: they can browse it, download from it and upload into it. */
    share?: string | null,
  ): Promise<HostHandle>;
}

export class Connection {
  /** The `host:port` this session is connected to. */
  readonly address: string;
  /** Lists a folder in the host's shared folder ('' is the folder itself). Rejects with the host's reason; the connection stays open. */
  listFiles(path: string): Promise<FileEntry[]>;
  /** Downloads to a local path, replacing what is there once the whole file has arrived intact. Resolves with the size in bytes. */
  downloadFile(remote: string, dest: string): Promise<number>;
  /** Uploads a local file. The host never replaces a file, so it may pick another name; resolves with the name it used. */
  uploadFile(src: string, remote: string): Promise<string>;
  /** Progress of the running transfer. Does not wait for the connection. */
  transferStatus(): TransferStatus;
  cancelTransfer(): void;
  readonly peer: PeerSummary;
  /** What the host currently allows this device to do. Refreshed by ping(). */
  readonly permissions: string[];
  /** Round trip time in milliseconds. */
  ping(): Promise<number>;
  /** Sends one mouse or keyboard event. Refusals come back later through takeNotices(). */
  sendInput(event: InputEvent): Promise<void>;
  /** Puts text on the host's clipboard. Needs "files" and a host with clipboard sharing on. Resolves with what the host said. */
  sendClipboard(text: string): Promise<string>;
  /** The host's clipboard text. `text` is missing, with the reason in `notice`, if the host refused or has no text. */
  getClipboard(): Promise<ClipboardReply>;
  /** Messages from the host for the person to read. Collected during ping(). */
  takeNotices(): Promise<string[]>;
  /** Sends a WebRTC offer, resolves with the host's answer. Needs the "view" permission. */
  requestVideo(offer: string): Promise<string>;
  /** Asks a wake relay to wake the computer it knows as `target`. Needs "power". Resolves with its answer. */
  wake(target: string): Promise<string>;
  /** The displays the host can stream one by one. Needs the "view" permission. */
  listRemoteDisplays(): Promise<Display[]>;
  /** Like requestVideo, for one display. Each display has its own connection. */
  requestDisplayVideo(display: number, offer: string): Promise<string>;
  close(): Promise<void>;
}

export interface ClipboardReply {
  /** Missing if the host refused or has no text on its clipboard. */
  text?: string;
  /** What the host said about it, if anything. */
  notice?: string;
}

export class HostHandle {
  /** The address actually bound, for example "127.0.0.1:7420". */
  readonly address: string;
  readonly pairingCode: string | null;
  /** Returns and clears what happened since the last call. */
  drainEvents(): HostEvent[];
  stop(): void;
}

export interface Display {
  id: number;
  name: string;
  width: number;
  height: number;
  /** Top left corner in the combined desktop. */
  x: number;
  y: number;
  primary: boolean;
}

/** The displays this computer can share, primary first. */
export function listDisplays(): Display[];
