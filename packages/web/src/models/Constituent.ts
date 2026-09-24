import { GitOrigin, Origin, RemoteOrigin } from "./Origin";

/**
 * The name a constituent is ingested under in the worker session, and so the
 * `file` every diagnostic, citation and LSP document refers to it by.
 *
 * `sumo` keeps the bare name. Other origins are namespaced by source
 * (`uploads/Merge.kif`, `example.com/Merge.kif`), so an upload or a URL can
 * share a repo file's name without colliding with it in the engine. Derived
 * on every boot and never persisted -- changing this scheme must bump the KB
 * snapshot cache's fingerprint (`services/kb-cache.ts`), since a snapshot
 * holds the names it was built with.
 */
export function engineFile(name: string, origin: Origin): string {
  switch (origin.kind) {
    case "sumo":
      return name;
    case "file":
      return `uploads/${name}`;
    case "url": {
      let host = "";
      try {
        host = new URL((origin as RemoteOrigin).url || name).host;
      } catch {
        /* not an absolute URL */
      }
      return `${host || "url"}/${name}`;
    }
  }
}

/** One loaded KIF file: its display name, where it came from, and the text
 *  last ingested for it. */
export class Constituent {
  name: string;
  origin: Origin;
  text: string;

  constructor(name: string, origin: Origin, text = "") {
    this.name = name;
    this.origin = origin;
    this.text = text;
  }

  /** See `engineFile`. */
  get file(): string {
    return engineFile(this.name, this.origin);
  }

  static defaults(): Constituent[] {
    return [
      new Constituent("english_format.kif", GitOrigin.default()),
      new Constituent("domainEnglishFormat.kif", GitOrigin.default()),
      new Constituent("ArabicCulture.kif", GitOrigin.default()),
      new Constituent("Anatomy.kif", GitOrigin.default()),
      new Constituent("Animals.kif", GitOrigin.default()),
      new Constituent("arteries.kif", GitOrigin.default()),
      new Constituent("Biography.kif", GitOrigin.default()),
      new Constituent("Cars.kif", GitOrigin.default()),
      new Constituent("Catalog.kif", GitOrigin.default()),
      new Constituent("ClimateStatecraft.kif", GitOrigin.default()),
      new Constituent("Communications.kif", GitOrigin.default()),
      new Constituent("ComputerInput.kif", GitOrigin.default()),
      new Constituent("ComputingBrands.kif", GitOrigin.default()),
      new Constituent("CountriesAndRegions.kif", GitOrigin.default()),
      new Constituent("Dining.kif", GitOrigin.default()),
      new Constituent("Economy.kif", GitOrigin.default()),
      new Constituent("emotion.kif", GitOrigin.default()),
      new Constituent("engineering.kif", GitOrigin.default()),
      new Constituent("Facebook.kif", GitOrigin.default()),
      new Constituent("FinancialOntology.kif", GitOrigin.default()),
      new Constituent("Food.kif", GitOrigin.default()),
      new Constituent("GeochronologicTimes.kif", GitOrigin.default()),
      new Constituent("Geography.kif", GitOrigin.default()),
      new Constituent("Government.kif", GitOrigin.default()),
      new Constituent("Hotel.kif", GitOrigin.default()),
      new Constituent("HouseholdAppliances.kif", GitOrigin.default()),
      new Constituent("Justice.kif", GitOrigin.default()),
      new Constituent("Languages.kif", GitOrigin.default()),
      new Constituent("Law.kif", GitOrigin.default()),
      new Constituent("Media.kif", GitOrigin.default()),
      new Constituent("Medicine.kif", GitOrigin.default()),
      new Constituent("Merge.kif", GitOrigin.default()),
      new Constituent("Mid-level-ontology.kif", GitOrigin.default()),
      new Constituent("MilitaryDevices.kif", GitOrigin.default()),
      new Constituent("Military.kif", GitOrigin.default()),
      new Constituent("MilitaryPersons.kif", GitOrigin.default()),
      new Constituent("MilitaryProcesses.kif", GitOrigin.default()),
      new Constituent("ModSim.kif", GitOrigin.default()),
      new Constituent("Music.kif", GitOrigin.default()),
      new Constituent("naics.kif", GitOrigin.default()),
      new Constituent("NavalModeling.kif", GitOrigin.default()),
      new Constituent("Objects.kif", GitOrigin.default()),
      new Constituent("People.kif", GitOrigin.default()),
      new Constituent("pictureList.kif", GitOrigin.default()),
      new Constituent("pictureList-ImageNet.kif", GitOrigin.default()),
      new Constituent("QoSontology.kif", GitOrigin.default()),
      new Constituent("Sports.kif", GitOrigin.default()),
      new Constituent("TransnationalIssues.kif", GitOrigin.default()),
      new Constituent("Transportation.kif", GitOrigin.default()),
      new Constituent("TransportDetail.kif", GitOrigin.default()),
      new Constituent("Trees.kif", GitOrigin.default()),
      new Constituent("UXExperimentalTerms.kif", GitOrigin.default()),
      new Constituent("VirusProteinAndCellPart.kif", GitOrigin.default()),
      new Constituent("Weather.kif", GitOrigin.default()),
      new Constituent("WMD.kif", GitOrigin.default()),
      new Constituent("WorldAirports.kif", GitOrigin.default()),
      new Constituent("capabilities.kif", GitOrigin.default()),
    ];
  }
}
