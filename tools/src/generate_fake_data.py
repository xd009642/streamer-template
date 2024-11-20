import argparse



if __name__ == "__main__":
    parser = argparse.ArgumentParser("generate_fake_data", description = "Generates a fake dataset using google tts")

    parser.add_argument("-o", "--out", type=str, help="folder to save the output files")
    parser.add_argument("-u", "--utterances", type=int, help="number of base utterances")
    parser.add_argument("-n", type=int, help="number of end files")
