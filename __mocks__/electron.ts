import { tmpdir } from 'os';

export const app = {
  getPath: (): string => tmpdir(),
};

export default { app };
