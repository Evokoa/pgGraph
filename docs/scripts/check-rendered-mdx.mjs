import { evaluate } from '@mdx-js/mdx';
import React from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import * as runtime from 'react/jsx-runtime';
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

function stubComponent(name) {
  return ({ children }) => React.createElement(
    'div',
    { 'data-doc-component': name },
    children,
  );
}

function standaloneSource(source) {
  return source
    .replace(/^import\s+.*$/gm, '')
    .replace(/withBasePath\(([^)]+)\)/g, '$1');
}

let failed = false;
for (const path of await mdxFiles(root)) {
  try {
    const source = standaloneSource(await readFile(path, 'utf8'));
    const names = [...source.matchAll(/<([A-Z][A-Za-z0-9]*)\b/g)].map((match) => match[1]);
    const components = Object.fromEntries([...new Set(names)].map((name) => [name, stubComponent(name)]));
    for (const match of source.matchAll(/<([A-Z][A-Za-z0-9]*)\.([A-Z][A-Za-z0-9]*)\b/g)) {
      components[match[1]][match[2]] = stubComponent(`${match[1]}.${match[2]}`);
    }
    const module = await evaluate(source, {
      ...runtime,
      useMDXComponents: () => components,
    });
    const html = renderToStaticMarkup(React.createElement(module.default, { components }));
    if (html.length < 32) {
      throw new Error('rendered page is missing content');
    }
  } catch (error) {
    failed = true;
    process.stderr.write(`${relative(root, path)}: ${error.message}\n`);
  }
}
if (failed) process.exit(1);
process.stdout.write('All public MDX pages render to static HTML successfully.\n');
