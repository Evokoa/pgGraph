import { compile } from '@mdx-js/mdx';
import { readdir, readFile } from 'node:fs/promises';
import { join, relative } from 'node:path';

const root = new URL('..', import.meta.url).pathname;

async function mdxFiles(directory) {
  const entries = await readdir(directory, { withFileTypes: true });
  const files = [];
  for (const entry of entries) {
    if (entry.name === 'node_modules') continue;
    const path = join(directory, entry.name);
    if (entry.isDirectory()) files.push(...await mdxFiles(path));
    else if (entry.name.endsWith('.mdx')) files.push(path);
  }
  return files;
}

let failed = false;
for (const path of await mdxFiles(root)) {
  try {
    await compile(await readFile(path, 'utf8'), { development: false });
  } catch (error) {
    failed = true;
    process.stderr.write(`${relative(root, path)}: ${error.message}\n`);
  }
}
if (failed) process.exit(1);
process.stdout.write('All public MDX pages compile successfully.\n');
