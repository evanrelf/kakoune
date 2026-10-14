#include "locale_glue.hh"

#include <clocale>
#include <cwctype>

namespace Kakoune::reference
{

bool init_locale()
{
    setlocale(LC_ALL, "");
    return iswalnum(L'é');
}

}
