import { useState, useEffect } from 'react';
import { CrdtStateProxy, WebSocketManager } from '@crdt-sync/core';
// @ts-ignore: Wasm module will be generated during the full build
import init, { WasmStateStore } from '@crdt-sync/core/pkg/web/crdt_sync.js';

export type CrdtStatus = 'connecting' | 'open' | 'error';

export interface UseCrdtStateResult<T> {
    state: T;
    proxy: CrdtStateProxy | null;
    status: CrdtStatus;
}

export function useCrdtState<T extends Record<string, unknown>>(
    url: string,
    initialState: T
): UseCrdtStateResult<T> {
    const [proxy, setProxy] = useState<CrdtStateProxy | null>(null);
    const [status, setStatus] = useState<CrdtStatus>('connecting');
    const [, setTick] = useState(0);

    // Note: we're ignoring initialState updates (this behaves like useState).
    const [initialRef] = useState(initialState);

    useEffect(() => {
        let active = true;
        let manager: WebSocketManager | null = null;
        let currentProxy: CrdtStateProxy | null = null;

        async function setup() {
            try {
                await init();
                if (!active) return;

                // Create a unique client ID
                const clientId = 'client-' + Math.random().toString(36).substring(2, 11);
                const store = new WasmStateStore(clientId);

                currentProxy = new CrdtStateProxy(store);

                // Initialize state
                for (const [key, value] of Object.entries(initialRef)) {
                    currentProxy.state[key] = value;
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
    }, [url, initialRef]);

    useEffect(() => {
        if (!proxy) return;

        // Subscribe to proxy updates to trigger React re-renders.
        return proxy.onUpdate(() => {
            setTick(t => t + 1);
        });
    }, [proxy]);

    const state = proxy ? (proxy.state as T) : initialRef;

    return { state, proxy, status };
}
