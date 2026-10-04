import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';
export default defineConfig({plugins:[react()],clearScreen:false,server:{watch:{ignored:['**/src-tauri/**','**/.tmp/**']}},test:{environment:'jsdom'}});