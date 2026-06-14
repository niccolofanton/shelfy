const { tmpdir } = require('os');

module.exports = {
  app: {
    getPath: () => tmpdir(),
  },
};
