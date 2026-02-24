import { useState, useEffect, useContext } from 'react';
import { CrdtStateProxy, WebSocketManager, initWasm, WasmStateStore } from '@crdt-sync/core';
import { CrdtSyncContext } from './CrdtSyncContext.js';

export type CrdtStatus = 'connecting' | 'open' | 'error';

export interface UseCrdtStateResult<T extends Record<string, unknown>> {
    state: T;
    proxy: CrdtStateProxy<T> | null;
    status: CrdtStatus;
}

export interface UseCrdtStateOptions {
    wasmUrl?: string;
}

export function useCrdtState<T extends Record<string, unknown>>(
    url: string,
    initialState: T,
    options?: UseCrdtStateOptions
): UseCrdtStateResult<T> {
    const [proxy, setProxy] = useState<CrdtStateProxy<T> | null>(null);
    const [status, setStatus] = useState<CrdtStatus>('connecting');
    const [, setTick] = useState(0);

    // Note: we're ignoring initialState updates (this behaves like useState).
    const [initialRef] = useState(initialState);

    const { wasmUrl: contextWasmUrl } = useContext(CrdtSyncContext);

    useEffect(() => {
        let active = true;
        let manager: WebSocketManager | null = null;
        let currentProxy: CrdtStateProxy<T> | null = null;

        async function setup() {
            try {
                await initWasm(options?.wasmUrl ?? contextWasmUrl);
                if (!active) return;

                // Create a unique client ID
                const clientId = 'client-' + Math.random().toString(36).substring(2, 11);
                const store = new WasmStateStore(clientId);

                currentProxy = new CrdtStateProxy<T>(store);

                // Initialize state. Cast needed: TypeScript disallows index-writes on a
                // generic T even though T extends Record<string, unknown>.
                const mutableState = currentProxy.state as Record<string, unknown>;
                for (const [key, value] of Object.entries(initialRef)) {
                    mutableState[key] = value;
                }

                if (!active) return;
                setProxy(currentProxy);

                const ws = new WebSocket(url);

                ws.onopen = () => {
                    if (active) setStatus('open');
                };

                ws.onerror = () => {
                    if (active) setStatus('error');
                };

                ws.onclose = () => {
                    if (active) setStatus('connecting');
                };

                manager = new WebSocketManager(store, currentProxy, ws as any);
            } catch (err) {
                console.error('Failed to initialize CRDT sync:', err);
                if (active) setStatus('error');
            }
        }

        setup();

        return () => {
            active = false;
            if (manager) manager.disconnect();
        };
    }, [url, initialRef, contextWasmUrl]);

    useEffect(() => {
        if (!proxy) return;

        // Re-render on any state change: local writes and incoming remote updates.
        return proxy.onChange(() => {
            setTick(t => t + 1);
        });
    }, [proxy]);

    const state = proxy ? (proxy.state as T) : initialRef;

    return { state, proxy, status };
}
