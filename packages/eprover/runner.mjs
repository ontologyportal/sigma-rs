/** Run one E executable in a fresh Emscripten instance. */
export async function runE(tptp, encodedArgs, program = "eprover", baseUrl = import.meta.url) {
  if (!["eprover", "e_axfilter"].includes(program)) throw new Error("Unknown E executable");
  const { default: createModule } = await import(new URL(`${program}.js`, baseUrl).href);
  const stdout = [];
  const stderr = [];
  let code = 0;
  const module = await createModule({
    noInitialRun: true,
    print: (line) => stdout.push(line),
    printErr: (line) => stderr.push(line),
    onExit: (status) => { code = status; },
  });
  module.FS.writeFile("/input.p", tptp);
  let args = JSON.parse(encodedArgs);
  let limit = 0;
  if (program === "e_axfilter") {
    const { budget, subsetLimit } = args;
    if (!Number.isInteger(budget) || budget < 1 || budget > 2147483647 ||
        !Number.isInteger(subsetLimit) || subsetLimit < 1 || subsetLimit > 1000) {
      throw new Error("Invalid axiom target or subset limit");
    }
    limit = subsetLimit;
    module.FS.writeFile("/filters",
      `narrow=GSinE(CountFormulas,hypos,false,1.2,,, ${budget},1.0)
` +
      `broad=GSinE(CountFormulas,hypos,false,5.0,,, ${budget},1.0)
`);
    args = ["--tptp3-format", "--seed-symbols=pfc", "--seed-method=a",
      `--seed-subsample=m${limit}`, "--filter=/filters"];
  }
  if (!Array.isArray(args) || !args.every((arg) => typeof arg === "string")) {
    throw new Error("Expected an E argument array");
  }
  const previousExitCode = globalThis.process?.exitCode;
  try {
    module.callMain([...args, "/input.p"]);
  } finally {
    if (globalThis.process) globalThis.process.exitCode = previousExitCode;
  }
  const result = { stdout: stdout.join("\n"), stderr: stderr.join("\n"), code };
  if (program === "e_axfilter") {
    if (code !== 0) throw new Error(result.stderr || result.stdout || `e_axfilter exited with ${code}`);
    const files = module.FS.readdir("/").filter((name) => name.endsWith(".p") && name !== "input.p").sort();
    const unique = new Map();
    for (const name of files) {
      const text = module.FS.readFile(`/${name}`, { encoding: "utf8" })
        .replace(/^((?:fof|cnf|tff)\([^,]+,)\s*hypothesis,/gm, "$1axiom,");
      const key = text.split("\n").filter((line) => line.trim() && !line.startsWith("%")).sort().join("\n");
      if (key && !unique.has(key)) unique.set(key, text);
    }
    result.subsets = [...unique.values()].slice(0, limit);
    result.stderr += `\nGenerated ${files.length} files, ${unique.size} distinct subsets; queued ${result.subsets.length}. Seed sampling and axiom targets are heuristic, not exhaustive coverage or hard size limits.`;
  }
  return result;
}
