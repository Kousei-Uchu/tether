export interface PeerSummary {
  name: string;
  platform: string;
  fingerprint: string;
  /** On a trust list: what the device may do here. On a connection: what the host allows this device to do. */
  permissions: string[];
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

export type PermissionName = 'view' | 'input' | 'media' | 'power' | 'files';

/** This machine's identity and trust store. */
export class Device {
  /** `home` overrides the data directory. */
  constructor(home?: string | null, name?: string | null);
  readonly name: string;
  readonly fingerprint: string;
  readonly home: string;
  peers(): PeerSummary[];
  grant(query: string, permissions: string[]): PeerSummary[];
  revoke(query: string, permissions: string[]): PeerSummary[];
  /** Returns how many devices were unpaired. */
  remove(query: string): number;
  pair(address: string, code: string): Promise<PeerSummary>;
  connect(address: string): Promise<Connection>;
  listen(bind: string, pairing?: PairingOptions | null): Promise<HostHandle>;
}

export class Connection {
  readonly peer: PeerSummary;
  /** What the host currently allows this device to do. Refreshed by ping(). */
  readonly permissions: string[];
  /** Round trip time in milliseconds. */
  ping(): Promise<number>;
  close(): Promise<void>;
}

export class HostHandle {
  /** The address actually bound, for example "127.0.0.1:7420". */
  readonly address: string;
  readonly pairingCode: string | null;
  /** Returns and clears what happened since the last call. */
  drainEvents(): HostEvent[];
  stop(): void;
}
