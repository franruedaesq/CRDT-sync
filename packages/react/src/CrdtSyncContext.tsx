import React, { createContext } from 'react';
import type { ReactNode } from 'react';

export interface CrdtSyncContextValue {
    wasmUrl?: string;
}

export const CrdtSyncContext = createContext<CrdtSyncContextValue>({});

export interface CrdtSyncProviderProps {
    wasmUrl?: string;
    children: ReactNode;
}

export function CrdtSyncProvider({ wasmUrl, children }: CrdtSyncProviderProps): JSX.Element {
    return (
        <CrdtSyncContext.Provider value={{ wasmUrl }}>
            {children}
        </CrdtSyncContext.Provider>
    );
}
