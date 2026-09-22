import { GitOrigin, Origin } from "./Origin";

/** One loaded KIF file: its worker-session name, where it came from, and the
 *  text last ingested for it. */
export class Constituent {
  name: string;
  origin: Origin;
  text: string;

  constructor(name: string, origin: Origin, text = "") {
    this.name = name;
    this.origin = origin;
    this.text = text;
  }

  static defaults(): Constituent[] {
    return [
      new Constituent("english_format.kif", GitOrigin.default()),
      new Constituent("domainEnglishFormat.kif", GitOrigin.default()),
      new Constituent("ArabicCulture.kif", GitOrigin.default()),
      new Constituent("Anatomy.kif", GitOrigin.default()),
      new Constituent("arteries.kif", GitOrigin.default()),
      new Constituent("Biography.kif", GitOrigin.default()),
      new Constituent("Cars.kif", GitOrigin.default()),
      new Constituent("Catalog.kif", GitOrigin.default()),
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
      new Constituent("Geography.kif", GitOrigin.default()),
      new Constituent("Government.kif", GitOrigin.default()),
      new Constituent("Hotel.kif", GitOrigin.default()),
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
      new Constituent("Music.kif", GitOrigin.default()),
      new Constituent("naics.kif", GitOrigin.default()),
      new Constituent("People.kif", GitOrigin.default()),
      new Constituent("pictureList.kif", GitOrigin.default()),
      new Constituent("pictureList-ImageNet.kif", GitOrigin.default()),
      new Constituent("QoSontology.kif", GitOrigin.default()),
      new Constituent("Sports.kif", GitOrigin.default()),
      new Constituent("TransnationalIssues.kif", GitOrigin.default()),
      new Constituent("Transportation.kif", GitOrigin.default()),
      new Constituent("TransportDetail.kif", GitOrigin.default()),
      new Constituent("UXExperimentalTerms.kif", GitOrigin.default()),
      new Constituent("VirusProteinAndCellPart.kif", GitOrigin.default()),
      new Constituent("Weather.kif", GitOrigin.default()),
      new Constituent("WMD.kif", GitOrigin.default()),
      new Constituent("capabilities.kif", GitOrigin.default()),
    ];
  }
}
