# typedoc-plugin-coverage

A plugin for TypeDoc to generate a documentation coverage badge.

This plugin will write a `coverage.svg` badge and/or `coverage.json` to your output directory when generating HTML documentation that
includes the percentage of your API surface which is documented. It will respect TypeDoc's `requiredToBeDocumented` option,
and only report missing documentation if reflections covered by that option are undocumented.

## Options

- `coverageLabel` - Define the label for the coverage badge. Defaults to 'document'.
- `coverageColor` - Define the define the color of the coverage badge background. Defaults to a dynamic color depending on coverage percentage.
- `coverageOutputPath` - Defines the path where the coverage badge will be written, defaults to `<output directory>/coverage.svg`.
- `coverageOutputType` - Defines the type of the coverage file to be written ('svg', 'json', 'all'). Defaults to 'svg'.
- `coverageSvgWidth` - Defines the width, in pixels, of the generated svg file.

Default colors/icon sourced from [esdoc-coverage-plugin](https://github.com/esdoc/esdoc-plugins/tree/master/esdoc-coverage-plugin)

If the numbers don't match what you expected, or don't match what TypeDoc implies should be documented, set `--logLevel Verbose` to see
additional logging about what was considered documented/not documented.
