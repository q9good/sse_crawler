# This is a sample Python script.

# Press Shift+F10 to execute it or replace it with your code.
# Press Double Shift to search everywhere for classes, files, tool windows, actions, and settings.
import pdfplumber
import os

download_folder = r"/home/revo/workspace/rust/sse_crawler/Download"
test_folder = r"/home/q9good/rust/sse_crawler/Download/上海合合信息科技股份有限公司"

def convert_pdf(pdf, txt):
    # Use a breakpoint in the code line below to debug your script.
    if os.path.exists(txt):
        return
    with open(txt, "w") as f:
        with pdfplumber.open(pdf) as pdf:
            for page in pdf.pages:
                text = page.extract_text()
                f.write(text)


# Press the green button in the gutter to run the script.
if __name__ == '__main__':
    g = os.walk(download_folder)
    # g = os.walk(test_folder)
    for path, dir_list, file_list in g:
        file_list.sort()
        txt_path = path.replace("Download", "txt")
        print(f"processing {path}")
        if not os.path.exists(txt_path):
            os.makedirs(txt_path)
        for file_name in file_list:
            pdf_file = os.path.join(path, file_name)
            txt_file = os.path.join(txt_path, file_name.replace(".pdf", ".txt"))
            convert_pdf(pdf_file, txt_file)

# See PyCharm help at https://www.jetbrains.com/help/pycharm/
